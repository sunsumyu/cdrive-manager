//! 风险评估模型
//!
//! 0-100 分制的风险评估，支持静态+动态+时效+依赖四层修正。

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

// ============================================================
// 风险等级
// ============================================================

/// 风险等级，基于最终分数（0-100）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RiskLevel {
    /// 0-10：安全删除（绿色）。
    Safe,
    /// 11-30：大概率安全（蓝色）。
    Low,
    /// 31-60：需谨慎（黄色）。
    Medium,
    /// 61-90：风险高（橙色）。
    High,
    /// 91-100：绝不可删除（红色）。
    Critical,
}

impl RiskLevel {
    pub fn from_score(score: u8) -> Self {
        match score {
            0..=10 => Self::Safe,
            11..=30 => Self::Low,
            31..=60 => Self::Medium,
            61..=90 => Self::High,
            _ => Self::Critical,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Safe => "安全",
            Self::Low => "低",
            Self::Medium => "中",
            Self::High => "高",
            Self::Critical => "关键",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }

    /// UI 颜色索引（用于颜色选择）。
    pub fn color_index(self) -> usize {
        match self {
            Self::Safe => 0,
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::Critical => 4,
        }
    }
}

// ============================================================
// 决策建议
// ============================================================

/// 对文件的操作建议。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Recommendation {
    /// 可安全删除。
    Delete,
    /// 强烈建议保留。
    Keep,
    /// 需要进一步确认（AI/用户介入）。
    Review,
    /// 暂不确定。
    Unknown,
}

impl Recommendation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Delete => "删除",
            Self::Keep => "保留",
            Self::Review => "需确认",
            Self::Unknown => "未知",
        }
    }
}

// ============================================================
// 决策来源
// ============================================================

/// 决策来源于哪个层级。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DecisionSource {
    /// 静态规则引擎。
    StaticRule,
    /// 动态感知引擎。
    DynamicPerception,
    /// AI 语义理解层。
    AiAssist,
    /// 用户手动确认。
    UserConfirmed,
    /// 未知（默认）。
    Unknown,
}

impl DecisionSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::StaticRule => "静态规则",
            Self::DynamicPerception => "动态感知",
            Self::AiAssist => "AI 辅助",
            Self::UserConfirmed => "用户确认",
            Self::Unknown => "未知",
        }
    }
}

// ============================================================
// 风险评估结果
// ============================================================

/// 单个文件的完整风险评估。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskAssessment {
    /// 基础风险分（来自 FileImportance）。
    pub base_score: u8,
    /// 动态修正（运行时状态）。
    pub dynamic_adjustment: i8,
    /// 时效修正（文件年龄）。
    pub age_adjustment: i8,
    /// 依赖修正（被多少组件依赖）。
    pub dependency_adjustment: i8,
    /// 最终风险分（0-100）。
    pub final_score: u8,
    /// 风险等级。
    pub level: RiskLevel,
    /// 删除影响描述（人类可读）。
    pub impact_description: String,
    /// 决策建议。
    pub recommendation: Recommendation,
    /// 决策依据。
    pub decision_source: DecisionSource,
}

impl RiskAssessment {
    /// 使用基础分数构建初始评估（无修正）。
    pub fn from_base(base_score: u8) -> Self {
        let final_score = base_score.min(100);
        let level = RiskLevel::from_score(final_score);
        Self {
            base_score,
            dynamic_adjustment: 0,
            age_adjustment: 0,
            dependency_adjustment: 0,
            final_score,
            level,
            impact_description: String::new(),
            recommendation: Recommendation::Unknown,
            decision_source: DecisionSource::Unknown,
        }
    }

    /// 添加动态修正。
    pub fn with_dynamic(mut self, adjustment: i8) -> Self {
        self.dynamic_adjustment = adjustment;
        self.recalculate();
        self
    }

    /// 添加时效修正。
    pub fn with_age(mut self, adjustment: i8) -> Self {
        self.age_adjustment = adjustment;
        self.recalculate();
        self
    }

    /// 添加依赖修正。
    pub fn with_dependency(mut self, adjustment: i8) -> Self {
        self.dependency_adjustment = adjustment;
        self.recalculate();
        self
    }

    /// 设置影响描述。
    pub fn with_impact(mut self, desc: impl Into<String>) -> Self {
        self.impact_description = desc.into();
        self
    }

    /// 设置决策建议。
    pub fn with_recommendation(mut self, rec: Recommendation) -> Self {
        self.recommendation = rec;
        self
    }

    /// 设置决策来源。
    pub fn with_source(mut self, source: DecisionSource) -> Self {
        self.decision_source = source;
        self
    }

    /// 重新计算 final_score 和 level。
    fn recalculate(&mut self) {
        let raw = self.base_score as i16
            + self.dynamic_adjustment as i16
            + self.age_adjustment as i16
            + self.dependency_adjustment as i16;
        self.final_score = raw.clamp(0, 100) as u8;
        self.level = RiskLevel::from_score(self.final_score);
    }

    /// 是否可以安全删除（Score <= 30）。
    pub fn is_safe_to_delete(&self) -> bool {
        self.final_score <= 30
    }
}

// ============================================================
// 动态修正因子
// ============================================================

/// 文件正被进程占用。
pub const ADJ_FILE_IN_USE: i8 = 15;
/// 文件被服务依赖。
pub const ADJ_SERVICE_DEPENDENCY: i8 = 20;
/// 文件是注册表指向的关键路径。
pub const ADJ_REGISTRY_CRITICAL: i8 = 25;
/// 文件是硬链接之一（多路径共享）。
pub const ADJ_HARDLINK: i8 = 10;
/// 文件最近 24h 内被修改。
pub const ADJ_RECENTLY_MODIFIED: i8 = 10;
/// 文件超过 30 天未修改（减分）。
pub const ADJ_STALE_FILE: i8 = -10;
/// 文件属于已卸载的应用（减分）。
pub const ADJ_UNINSTALLED_APP: i8 = -20;

// ============================================================
// 时效修正计算
// ============================================================

/// 根据文件修改时间计算时效修正。
pub fn compute_age_adjustment(modified: Option<SystemTime>, _now: SystemTime) -> i8 {
    let modified = match modified {
        Some(m) => m,
        None => return 0,
    };

    let age = match _now.duration_since(modified) {
        Ok(d) => d,
        Err(_) => return 0,
    };

    if age.as_secs() < 24 * 3600 {
        // 24 小时内
        ADJ_RECENTLY_MODIFIED
    } else if age.as_secs() > 30 * 24 * 3600 {
        // 超过 30 天
        ADJ_STALE_FILE
    } else {
        0
    }
}
