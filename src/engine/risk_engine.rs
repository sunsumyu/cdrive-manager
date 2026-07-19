//! 统一风险决策引擎
//!
//! 三层架构的决策中心，整合静态规则、动态感知和 AI 辅助。
//! 所有文件操作统一通过此引擎决策。

use std::path::Path;
use std::sync::Arc;

use crate::engine::ai_assist::AiAssistEngine;
use crate::engine::classification::{FileCategory, FileClassification, FileImportance};
use crate::engine::knowledge_base::KnowledgeBase;
use crate::engine::risk_assessment::{
    DecisionSource, Recommendation, RiskAssessment, RiskLevel,
};
use crate::engine::static_rules::StaticRuleEngine;

// ============================================================
// 统一决策引擎
// ============================================================

/// 文件分析结果。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AnalysisResult {
    /// 文件分类信息。
    pub classification: FileClassification,
    /// 风险评估结果。
    pub risk: RiskAssessment,
    /// 删除影响描述（人类可读）。
    pub impact_description: String,
    /// 动态感知快照。
    pub dynamic_snapshot: Option<crate::engine::dynamic_perception::DynamicSnapshot>,
}

/// 统一风险决策引擎。
///
/// 这是文件操作决策的唯一入口，所有删除/清理/迁移操作
/// 都必须通过此引擎获取决策。
#[derive(Debug, Clone)]
pub struct RiskEngine {
    static_engine: StaticRuleEngine,
    #[allow(dead_code)]
    dynamic_engine: crate::engine::dynamic_perception::DynamicPerceptionEngine,
    ai_engine: AiAssistEngine,
    /// 知识库缓存，避免重复分析。
    knowledge_base: Option<Arc<KnowledgeBase>>,
}

impl Default for RiskEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl RiskEngine {
    /// 创建引擎实例。
    pub fn new() -> Self {
        Self {
            static_engine: StaticRuleEngine::new(),
            dynamic_engine: crate::engine::dynamic_perception::DynamicPerceptionEngine::new(),
            ai_engine: AiAssistEngine::new(),
            knowledge_base: None,
        }
    }

    /// 创建带知识库缓存的引擎实例。
    pub fn with_kb(kb: Arc<KnowledgeBase>) -> Self {
        Self {
            static_engine: StaticRuleEngine::new(),
            dynamic_engine: crate::engine::dynamic_perception::DynamicPerceptionEngine::new(),
            ai_engine: AiAssistEngine::new(),
            knowledge_base: Some(kb),
        }
    }

    /// 对文件进行完整分析。
    ///
    /// 分析流程：
    /// 1. 查询知识库缓存（启用时）
    /// 2. 静态规则分类 → 基础风险分
    /// 3. 动态感知修正（运行时探测）
    /// 4. AI 辅助分析（当静态规则无法匹配时）
    /// 5. 时效修正（预留）
    /// 6. 依赖修正（预留）
    /// 7. 生成最终决策
    pub fn analyze(&self, path: &Path) -> Option<AnalysisResult> {
        // Step 0: 查询知识库缓存（避免重复分析）
        if let Some(ref kb) = self.knowledge_base {
            if let Ok(meta) = std::fs::metadata(path) {
                if let Ok(modified) = meta.modified() {
                    let modified_secs = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if let Some(cached) = kb.get(path, modified_secs) {
                        return Some(cached);
                    }
                }
            }
        }

        // Step 1: 尝试静态规则分类
        let static_result = self.static_engine.classify(path);
        let classification = static_result.as_ref().map(|r| r.classification.clone());

        let result = if let Some(ref static_result) = static_result {
            // 静态规则匹配到了
            let classification = static_result.classification.clone();
            let base_score = classification.importance.base_score();
            let mut risk = RiskAssessment::from_base(base_score)
                .with_source(DecisionSource::StaticRule);

            // Step 2: 动态感知修正
            let dynamic_adjustment = self.dynamic_engine.compute_adjustment(path);
            if dynamic_adjustment != 0 {
                risk = risk.with_dynamic(dynamic_adjustment);
                risk = risk.with_source(DecisionSource::DynamicPerception);
            }

            // Step 3: 动态感知快照
            let dynamic_snapshot = if cfg!(windows) {
                Some(self.dynamic_engine.snapshot(path))
            } else {
                None
            };

            // Step 4: 生成决策
            let recommendation = self.compute_recommendation(&classification, &risk);
            risk = risk.with_recommendation(recommendation);

            let impact_description = self.describe_impact(&classification, &risk);
            risk = risk.with_impact(&impact_description);

            AnalysisResult {
                classification,
                risk,
                impact_description,
                dynamic_snapshot,
            }
        } else {
            // Step 5: 静态规则未匹配，调用 AI 辅助分析
            self.ai_engine.analyze_unknown_file(path)?
        };

        // Step 6: 写入知识库缓存（仅在启用时）
        if let Some(ref kb) = self.knowledge_base {
            if let Ok(meta) = std::fs::metadata(path) {
                if let Ok(modified) = meta.modified() {
                    let modified_secs = modified
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let _ = kb.put(path, modified_secs, &result);
                }
            }
        }

        Some(result)
    }

    /// 判断文件是否可以安全删除。
    ///
    /// 安全标准：final_score <= 30 且不在系统核心保护路径中。
    pub fn can_delete(&self, path: &Path) -> SafetyVerdict {
        // Step 1: 先尝试通过静态规则分类
        match self.analyze(path) {
            Some(result) => {
                // 静态规则匹配到了，基于分类和评分决策
                if result.risk.is_safe_to_delete() {
                    SafetyVerdict::Safe
                } else {
                    SafetyVerdict::Unsafe(result.impact_description)
                }
            }
            None => {
                // 静态规则未匹配到，检查是否在核心保护路径中
                if self.static_engine.is_protected_path(path) {
                    SafetyVerdict::Unsafe("路径位于系统保护区域".to_string())
                } else {
                    SafetyVerdict::Unknown
                }
            }
        }
    }

    /// 生成删除影响描述。
    fn describe_impact(&self, classification: &FileClassification, risk: &RiskAssessment) -> String {
        let category_desc = match classification.category {
            FileCategory::SystemBinary | FileCategory::SystemDriver | FileCategory::ComponentStore => {
                return "删除会导致系统崩溃或无法启动".to_string();
            }
            FileCategory::InstallerCache => {
                return "删除会导致已安装应用无法卸载或修复".to_string();
            }
            FileCategory::SystemTemp | FileCategory::WindowsUpdateCache | FileCategory::DeliveryOptimization => {
                return "可安全删除，下次需要时自动重建".to_string();
            }
            FileCategory::PrefetchData => {
                return "删除后应用启动可能略慢，系统会自动重建".to_string();
            }
            FileCategory::BrowserCache | FileCategory::AppCache => {
                return "删除后应用会自动重建缓存，无数据丢失".to_string();
            }
            FileCategory::BuildArtifact => {
                return "删除后需重新构建，源码不受影响".to_string();
            }
            FileCategory::PackageManagerCache => {
                return "删除后下次安装需重新下载依赖".to_string();
            }
            FileCategory::ErrorReport => {
                return "删除后丢失历史崩溃报告，无功能影响".to_string();
            }
            FileCategory::MemoryDump => {
                return "删除后丢失诊断转储，无功能影响".to_string();
            }
            FileCategory::GenericLog | FileCategory::SystemLog | FileCategory::AppLog => {
                return "删除后丢失日志历史，无功能影响".to_string();
            }
            _ => {}
        };

        let _ = category_desc; // 避免 unused variable warning

        // 通用描述
        match classification.importance {
            FileImportance::Critical => "删除会导致系统崩溃或无法启动".to_string(),
            FileImportance::Essential => "删除会导致应用无法运行或数据丢失".to_string(),
            FileImportance::Performance => "删除后性能可能下降，但功能正常".to_string(),
            FileImportance::Optional => "删除后自动重建，无持久影响".to_string(),
            FileImportance::Disposable => "可安全删除，无任何影响".to_string(),
        }
    }

    /// 根据分类和风险计算决策建议。
    fn compute_recommendation(
        &self,
        classification: &FileClassification,
        risk: &RiskAssessment,
    ) -> Recommendation {
        // 关键或重要的文件 → 保留
        if matches!(classification.importance, FileImportance::Critical | FileImportance::Essential) {
            return Recommendation::Keep;
        }

        // 风险分高 → 需要确认
        if risk.final_score > 60 {
            return Recommendation::Review;
        }

        // 可丢弃的文件 → 删除
        if matches!(classification.importance, FileImportance::Disposable) {
            return Recommendation::Delete;
        }

        // 可选或性能影响的 → 根据分数判断
        if risk.final_score <= 30 {
            Recommendation::Delete
        } else if risk.final_score <= 60 {
            Recommendation::Review
        } else {
            Recommendation::Keep
        }
    }
}

// ============================================================
// 安全判决
// ============================================================

/// 对文件删除操作的安全判决。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafetyVerdict {
    /// 可以安全删除。
    Safe,
    /// 不安全，附带原因。
    Unsafe(String),
    /// 不确定，需要进一步确认。
    Unknown,
}

impl SafetyVerdict {
    /// 是否安全。
    pub fn is_safe(&self) -> bool {
        matches!(self, Self::Safe)
    }

    /// 是否不安全。
    pub fn is_unsafe(&self) -> bool {
        matches!(self, Self::Unsafe(_))
    }

    /// 是否未知。
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}
