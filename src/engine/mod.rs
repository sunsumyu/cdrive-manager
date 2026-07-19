//! 系统文件感知引擎
//!
//! 三层架构的统一决策中心：
//! - 静态规则层：路径前缀/扩展名/组件名匹配 → 确定性分类
//! - 动态感知层：运行时 WMI/注册表/服务/进程探测 → 实时修正
//! - AI 语义层：LLM 辅助理解未知文件 → 深度推理
//!
//! 所有模块通过 `RiskEngine` 统一决策。

pub mod ai_assist;
pub mod classification;
pub mod knowledge_base;
pub mod risk_assessment;
pub mod risk_engine;
pub mod static_rules;
pub mod dynamic_perception;
pub mod wmi_query;
pub mod registry_reader;
pub mod service_query;
pub mod process_query;
pub mod file_handle;

#[cfg(test)]
mod tests;

pub use ai_assist::AiAssistEngine;
pub use classification::{FileClassification, FileCategory, FileImportance, SystemLayer};
pub use risk_assessment::{DecisionSource, Recommendation, RiskAssessment, RiskLevel};
pub use risk_engine::{RiskEngine, AnalysisResult, SafetyVerdict};
pub use knowledge_base::KnowledgeBase;
pub use static_rules::StaticRuleEngine;
pub use dynamic_perception::DynamicPerceptionEngine;
