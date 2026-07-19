//! AI 辅助引擎
//!
//! 当静态规则和动态感知均无法识别文件时，通过 LLM 进行语义分析。
//! 结果缓存到本地知识库，避免重复查询。

use std::path::Path;

use crate::engine::classification::{FileCategory, FileClassification, FileImportance};
use crate::engine::risk_assessment::{DecisionSource, RiskAssessment};
use crate::engine::risk_engine::AnalysisResult;

/// AI 辅助引擎。
#[derive(Debug, Clone)]
pub struct AiAssistEngine {
    // TODO: 配置 LLM API 端点和密钥
}

impl AiAssistEngine {
    pub fn new() -> Self {
        Self {}
    }

    /// 对未知文件进行语义分析。
    ///
    /// 提供上下文信息（路径、扩展名、父目录等）给 AI，
    /// 让 AI 推断文件类型和删除影响。
    pub fn analyze_unknown_file(
        &self,
        path: &Path,
    ) -> Option<AnalysisResult> {
        // 构建上下文信息
        let context = build_analysis_context(path);

        // Step 1: 基于启发式规则快速分类（无需 LLM）
        let heuristic_result = self.heuristic_classify(path, &context);
        if heuristic_result.is_some() {
            return heuristic_result;
        }

        // Step 2: 真正的 LLM 语义分析（需要 API key 和网络）
        // TODO: 接入 LLM API（如 OpenAI、Claude、本地 Ollama）
        // 当前版本：返回 Unknown 分类，保守策略
        Some(create_unknown_analysis(path, &context))
    }

    /// 基于启发式的快速分类（无需网络）。
    fn heuristic_classify(&self, path: &Path, _context: &AnalysisContext) -> Option<AnalysisResult> {
        let path_str = path.to_string_lossy().to_ascii_lowercase();
        let file_name = path.file_name()?.to_string_lossy().to_ascii_lowercase();

        // 启发式 1：文件名关键词
        if file_name.contains("temp") || file_name.contains("tmp") || file_name.contains("cache") {
            return Some(create_analysis_result(path, FileCategory::GenericTemp, FileImportance::Disposable));
        }
        if file_name.contains("log") {
            return Some(create_analysis_result(path, FileCategory::GenericLog, FileImportance::Optional));
        }
        if file_name.contains("backup") || file_name.contains(".bak") {
            return Some(create_analysis_result(path, FileCategory::GenericBackup, FileImportance::Optional));
        }
        if file_name.contains("crash") || file_name.contains("dump") {
            return Some(create_analysis_result(path, FileCategory::CrashDump, FileImportance::Disposable));
        }

        // 启发式 2：路径关键词
        if path_str.contains("\\temp\\") || path_str.contains("\\tmp\\") {
            return Some(create_analysis_result(path, FileCategory::GenericTemp, FileImportance::Disposable));
        }
        if path_str.contains("\\cache\\") || path_str.contains("\\caches\\") {
            return Some(create_analysis_result(path, FileCategory::GenericCache, FileImportance::Optional));
        }
        if path_str.contains("\\logs\\") || path_str.contains("\\log\\") {
            return Some(create_analysis_result(path, FileCategory::GenericLog, FileImportance::Optional));
        }
        if path_str.contains("\\backup\\") || path_str.contains("\\backups\\") {
            return Some(create_analysis_result(path, FileCategory::GenericBackup, FileImportance::Optional));
        }
        if path_str.contains("\\download\\") || path_str.contains("\\downloads\\") {
            return Some(create_analysis_result(path, FileCategory::UserDownload, FileImportance::Optional));
        }

        // 启发式 3：扩展名
        if let Some(ext) = path.extension() {
            let ext = ext.to_string_lossy().to_ascii_lowercase();
            match ext.as_str() {
                "tmp" | "temp" => return Some(create_analysis_result(path, FileCategory::GenericTemp, FileImportance::Disposable)),
                "log" => return Some(create_analysis_result(path, FileCategory::GenericLog, FileImportance::Optional)),
                "bak" => return Some(create_analysis_result(path, FileCategory::GenericBackup, FileImportance::Optional)),
                "dmp" => return Some(create_analysis_result(path, FileCategory::CrashDump, FileImportance::Disposable)),
                "etl" => return Some(create_analysis_result(path, FileCategory::EventTrace, FileImportance::Optional)),
                "pid" => return Some(create_analysis_result(path, FileCategory::GenericTemp, FileImportance::Disposable)),
                _ => {}
            }
        }

        None
    }

    /// 调用 LLM API 进行语义分析（预留接口）。
    #[allow(dead_code)]
    fn call_llm_for_analysis(&self, _path: &Path, _context: &AnalysisContext) -> Option<AnalysisResult> {
        // TODO: 实现 LLM 调用
        // 1. 构建 prompt（包含静态+动态分析结果作为上下文）
        // 2. 调用 LLM API（OpenAI、Claude、本地 Ollama）
        // 3. 解析 JSON 响应
        // 4. 缓存到知识库
        None
    }
}

/// 分析上下文。
#[derive(Debug, Clone)]
struct AnalysisContext {
    extension: Option<String>,
    file_name: String,
    parent_components: Vec<String>,
    size_hint: Option<u64>,
}

fn build_analysis_context(path: &Path) -> AnalysisContext {
    let extension = path.extension()
        .map(|e| e.to_string_lossy().to_string());
    let file_name = path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let parent_components: Vec<String> = path.parent()
        .map(|p| p.components()
            .filter_map(|c| c.as_os_str().to_str())
            .skip(1) // skip drive letter
            .take(3)
            .map(|s| s.to_string())
            .collect())
        .unwrap_or_default();

    AnalysisContext {
        extension,
        file_name,
        parent_components,
        size_hint: None,
    }
}

/// 创建已知的分析结果。
fn create_analysis_result(path: &Path, category: FileCategory, importance: FileImportance) -> AnalysisResult {
    let classification = FileClassification::new(path, category, importance);
    let base_score = importance.base_score();
    let risk = RiskAssessment::from_base(base_score)
        .with_source(DecisionSource::AiAssist);

    let impact_description = format!("AI 辅助识别：{}", category.label());

    AnalysisResult {
        classification: classification.clone(),
        risk,
        impact_description: impact_description.clone(),
        dynamic_snapshot: None,
    }
}

/// 创建 Unknown 分析结果（保守策略）。
fn create_unknown_analysis(path: &Path, _context: &AnalysisContext) -> AnalysisResult {
    let classification = FileClassification::new(path, FileCategory::Unknown, FileImportance::Essential);
    let risk = RiskAssessment::from_base(70)
        .with_source(DecisionSource::AiAssist);

    AnalysisResult {
        classification: classification.clone(),
        risk,
        impact_description: "AI 无法识别此文件，保守策略建议保留".to_string(),
        dynamic_snapshot: None,
    }
}
