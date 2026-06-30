use std::{
    collections::HashMap,
    env,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use crossbeam_channel::{Receiver, unbounded};
use serde::{Deserialize, Serialize};

use crate::{
    cleanup::{CleanupPreview, CleanupRisk},
    duplicates::DuplicatePreview,
};

#[derive(Debug, Clone)]
pub struct AiProviderConfig {
    pub base_url: String,
    pub model: String,
    pub api_key_env: String,
    pub send_full_paths: bool,
    pub timeout_secs: u64,
    pub max_candidates: usize,
}

impl Default for AiProviderConfig {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com/v1".to_owned(),
            model: "gpt-4o-mini".to_owned(),
            api_key_env: "OPENAI_API_KEY".to_owned(),
            send_full_paths: false,
            timeout_secs: 60,
            max_candidates: 200,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiAnalysisOptions {
    pub root: PathBuf,
    pub cleanup_preview: Option<Arc<CleanupPreview>>,
    pub duplicate_preview: Option<Arc<DuplicatePreview>>,
    pub provider_config: AiProviderConfig,
}

#[derive(Debug)]
pub struct AiAnalysisHandle {
    pub receiver: Receiver<AiAnalysisEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl AiAnalysisHandle {
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone)]
pub enum AiAnalysisEvent {
    Progress(AiAnalysisProgress),
    Finished(AiAnalysisFinished),
}

#[derive(Debug, Clone)]
pub struct AiAnalysisProgress {
    pub report: Arc<AiAnalysisReport>,
    pub phase: AiAnalysisPhase,
    pub current_item: Option<String>,
    pub finished: bool,
    pub cancelled: bool,
}

#[derive(Debug, Clone)]
pub struct AiAnalysisFinished {
    pub report: Arc<AiAnalysisReport>,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAnalysisPhase {
    CollectingCandidates,
    Analyzing,
    Auditing,
    Finished,
}

impl AiAnalysisPhase {
    pub fn label(self) -> &'static str {
        match self {
            Self::CollectingCandidates => "收集候选",
            Self::Analyzing => "AI 分析",
            Self::Auditing => "AI 审核",
            Self::Finished => "完成",
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiAnalysisReport {
    pub root: PathBuf,
    pub provider_label: String,
    pub model: String,
    pub candidate_count: u64,
    pub delete_candidate_count: u64,
    pub needs_review_count: u64,
    pub rejected_count: u64,
    pub protected_count: u64,
    pub error_count: u64,
    pub findings: Vec<AiReviewFinding>,
    pub errors: Vec<String>,
}

impl AiAnalysisReport {
    fn empty(root: PathBuf, config: &AiProviderConfig) -> Self {
        Self {
            root,
            provider_label: provider_label(config),
            model: config.model.clone(),
            candidate_count: 0,
            delete_candidate_count: 0,
            needs_review_count: 0,
            rejected_count: 0,
            protected_count: 0,
            error_count: 0,
            findings: Vec::new(),
            errors: Vec::new(),
        }
    }

    fn from_findings(
        root: PathBuf,
        config: &AiProviderConfig,
        findings: Vec<AiReviewFinding>,
        errors: Vec<String>,
    ) -> Self {
        let delete_candidate_count = findings
            .iter()
            .filter(|finding| finding.is_delete_list_candidate())
            .count() as u64;
        let needs_review_count = findings
            .iter()
            .filter(|finding| {
                finding.audit_status == AiAuditStatus::NeedsHumanReview
                    || finding.final_recommendation == AiRecommendation::NeedsReview
            })
            .count() as u64;
        let rejected_count = findings
            .iter()
            .filter(|finding| {
                finding.audit_status == AiAuditStatus::Rejected
                    || finding.final_recommendation == AiRecommendation::Keep
            })
            .count() as u64;
        let protected_count = findings.iter().filter(|finding| finding.protected).count() as u64;
        let error_count = errors.len() as u64;

        Self {
            root,
            provider_label: provider_label(config),
            model: config.model.clone(),
            candidate_count: findings.len() as u64,
            delete_candidate_count,
            needs_review_count,
            rejected_count,
            protected_count,
            error_count,
            findings,
            errors,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AiReviewFinding {
    pub path: PathBuf,
    pub display_path: String,
    pub size: u64,
    pub source: AiFindingSource,
    pub category: AiCleanupCategory,
    pub risk: AiCleanupRisk,
    pub confidence: f32,
    pub protected: bool,
    pub analysis_recommendation: AiRecommendation,
    pub analysis_reason: String,
    pub audit_status: AiAuditStatus,
    pub audit_reason: String,
    pub final_recommendation: AiRecommendation,
}

impl AiReviewFinding {
    pub fn is_delete_list_candidate(&self) -> bool {
        matches!(
            self.audit_status,
            AiAuditStatus::Approved | AiAuditStatus::Corrected
        ) && self.final_recommendation == AiRecommendation::CandidateForDeletion
            && !self.protected
            && !matches!(self.risk, AiCleanupRisk::High | AiCleanupRisk::Unknown)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiFindingSource {
    CleanupPreview,
    DuplicateFile,
}

impl AiFindingSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::CleanupPreview => "清理预览",
            Self::DuplicateFile => "重复文件",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CleanupPreview => "cleanup_preview",
            Self::DuplicateFile => "duplicate_file",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiCleanupCategory {
    TempFile,
    CacheFile,
    LogOrDump,
    BackupOrOld,
    Duplicate,
    InstallerOrDownload,
    BuildArtifact,
    Unknown,
    Keep,
}

impl AiCleanupCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::TempFile => "临时文件",
            Self::CacheFile => "缓存文件",
            Self::LogOrDump => "日志/转储",
            Self::BackupOrOld => "备份/旧文件",
            Self::Duplicate => "重复文件",
            Self::InstallerOrDownload => "安装包/下载",
            Self::BuildArtifact => "构建产物",
            Self::Unknown => "未知",
            Self::Keep => "保留",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::TempFile => "temp_file",
            Self::CacheFile => "cache_file",
            Self::LogOrDump => "log_or_dump",
            Self::BackupOrOld => "backup_or_old",
            Self::Duplicate => "duplicate",
            Self::InstallerOrDownload => "installer_or_download",
            Self::BuildArtifact => "build_artifact",
            Self::Unknown => "unknown",
            Self::Keep => "keep",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::TempFile => 0,
            Self::CacheFile => 1,
            Self::LogOrDump => 2,
            Self::BackupOrOld => 3,
            Self::Duplicate => 4,
            Self::InstallerOrDownload => 5,
            Self::BuildArtifact => 6,
            Self::Unknown => 7,
            Self::Keep => 8,
        }
    }

    fn parse(value: &str) -> Self {
        match normalized_enum(value).as_str() {
            "temp_file" | "temp" => Self::TempFile,
            "cache_file" | "cache" => Self::CacheFile,
            "log_or_dump" | "log" | "dump" => Self::LogOrDump,
            "backup_or_old" | "backup" | "old" => Self::BackupOrOld,
            "duplicate" | "duplicate_file" => Self::Duplicate,
            "installer_or_download" | "installer" | "download" => Self::InstallerOrDownload,
            "build_artifact" | "build" => Self::BuildArtifact,
            "keep" => Self::Keep,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiCleanupRisk {
    Safe,
    Low,
    Medium,
    High,
    Unknown,
}

impl AiCleanupRisk {
    pub fn label(self) -> &'static str {
        match self {
            Self::Safe => "安全",
            Self::Low => "低",
            Self::Medium => "中",
            Self::High => "高",
            Self::Unknown => "未知",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Safe => "safe",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Unknown => "unknown",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Safe => 0,
            Self::Low => 1,
            Self::Medium => 2,
            Self::High => 3,
            Self::Unknown => 4,
        }
    }

    fn parse(value: &str) -> Self {
        match normalized_enum(value).as_str() {
            "safe" => Self::Safe,
            "low" => Self::Low,
            "medium" | "mid" => Self::Medium,
            "high" => Self::High,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiRecommendation {
    CandidateForDeletion,
    Keep,
    NeedsReview,
}

impl AiRecommendation {
    pub fn label(self) -> &'static str {
        match self {
            Self::CandidateForDeletion => "可作为待删候选",
            Self::Keep => "保留",
            Self::NeedsReview => "需人工复核",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::CandidateForDeletion => "candidate_for_deletion",
            Self::Keep => "keep",
            Self::NeedsReview => "needs_review",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::CandidateForDeletion => 0,
            Self::NeedsReview => 1,
            Self::Keep => 2,
        }
    }

    fn parse(value: &str) -> Self {
        match normalized_enum(value).as_str() {
            "candidate_for_deletion" | "delete" | "deletion_candidate" => {
                Self::CandidateForDeletion
            }
            "keep" | "reject" => Self::Keep,
            _ => Self::NeedsReview,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiAuditStatus {
    Approved,
    Corrected,
    Rejected,
    NeedsHumanReview,
}

impl AiAuditStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Approved => "已通过",
            Self::Corrected => "已纠正",
            Self::Rejected => "已拒绝",
            Self::NeedsHumanReview => "需人工复核",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Corrected => "corrected",
            Self::Rejected => "rejected",
            Self::NeedsHumanReview => "needs_human_review",
        }
    }

    pub fn rank(self) -> u8 {
        match self {
            Self::Approved => 0,
            Self::Corrected => 1,
            Self::NeedsHumanReview => 2,
            Self::Rejected => 3,
        }
    }

    fn parse(value: &str) -> Self {
        match normalized_enum(value).as_str() {
            "approved" | "approve" => Self::Approved,
            "corrected" | "correct" => Self::Corrected,
            "rejected" | "reject" => Self::Rejected,
            _ => Self::NeedsHumanReview,
        }
    }
}

#[derive(Debug, Clone)]
struct CandidateRecord {
    id: String,
    path: PathBuf,
    display_path: String,
    size: u64,
    source: AiFindingSource,
    category_hint: AiCleanupCategory,
    risk_hint: AiCleanupRisk,
    protected: bool,
    reason: String,
}

#[derive(Debug, Clone, Serialize)]
struct CandidatePayload {
    id: String,
    source: String,
    path: String,
    size_bytes: u64,
    protected: bool,
    category_hint: String,
    risk_hint: String,
    reason: String,
    extension: String,
    path_depth: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AiFindingPayload {
    id: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    risk: String,
    #[serde(default)]
    confidence: f32,
    #[serde(default)]
    recommendation: String,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AiFindingsEnvelope {
    findings: Vec<AiFindingPayload>,
}

#[derive(Debug, Clone, Deserialize)]
struct AuditFindingPayload {
    id: String,
    #[serde(default)]
    audit_status: String,
    #[serde(default)]
    final_recommendation: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    risk: String,
    #[serde(default)]
    confidence: Option<f32>,
    #[serde(default)]
    reason: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AuditFindingsEnvelope {
    findings: Vec<AuditFindingPayload>,
}

#[derive(Debug, Clone, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    temperature: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<ChatChoice>,
}

#[derive(Debug, Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

pub fn spawn_ai_analysis(options: AiAnalysisOptions) -> AiAnalysisHandle {
    let (sender, receiver) = unbounded();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel_flag = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        let config = options.provider_config.clone();
        let root = options.root.clone();
        let empty_report = Arc::new(AiAnalysisReport::empty(root.clone(), &config));
        let _ = sender.send(AiAnalysisEvent::Progress(AiAnalysisProgress {
            report: Arc::clone(&empty_report),
            phase: AiAnalysisPhase::CollectingCandidates,
            current_item: None,
            finished: false,
            cancelled: false,
        }));

        let candidates = collect_candidates(&options);
        let collected_report = Arc::new(AiAnalysisReport::from_findings(
            root.clone(),
            &config,
            fallback_findings(&candidates, "等待 AI 分析。"),
            Vec::new(),
        ));
        let _ = sender.send(AiAnalysisEvent::Progress(AiAnalysisProgress {
            report: Arc::clone(&collected_report),
            phase: AiAnalysisPhase::Analyzing,
            current_item: Some(format!("已收集 {} 个候选", candidates.len())),
            finished: false,
            cancelled: false,
        }));

        if worker_cancel_flag.load(Ordering::Relaxed) {
            finish_cancelled(&sender, collected_report);
            return;
        }

        let mut errors = Vec::new();
        if candidates.is_empty() {
            errors.push("没有可供 AI 分析的清理或重复文件候选。".to_owned());
            let report = Arc::new(AiAnalysisReport::from_findings(
                root,
                &config,
                Vec::new(),
                errors,
            ));
            let _ = sender.send(AiAnalysisEvent::Progress(AiAnalysisProgress {
                report: Arc::clone(&report),
                phase: AiAnalysisPhase::Finished,
                current_item: None,
                finished: true,
                cancelled: false,
            }));
            let _ = sender.send(AiAnalysisEvent::Finished(AiAnalysisFinished {
                report,
                cancelled: false,
            }));
            return;
        }

        let analyses = match request_analysis(&config, &candidates) {
            Ok(analyses) => analyses,
            Err(error) => {
                errors.push(format!("AI 分析失败：{:#}", error));
                let report = Arc::new(AiAnalysisReport::from_findings(
                    root,
                    &config,
                    fallback_findings(&candidates, "AI 分析失败，已转入人工复核。"),
                    errors,
                ));
                let _ = sender.send(AiAnalysisEvent::Progress(AiAnalysisProgress {
                    report: Arc::clone(&report),
                    phase: AiAnalysisPhase::Finished,
                    current_item: None,
                    finished: true,
                    cancelled: false,
                }));
                let _ = sender.send(AiAnalysisEvent::Finished(AiAnalysisFinished {
                    report,
                    cancelled: false,
                }));
                return;
            }
        };

        let analysis_report = Arc::new(AiAnalysisReport::from_findings(
            root.clone(),
            &config,
            findings_from_analysis(&candidates, &analyses),
            errors.clone(),
        ));
        let _ = sender.send(AiAnalysisEvent::Progress(AiAnalysisProgress {
            report: Arc::clone(&analysis_report),
            phase: AiAnalysisPhase::Auditing,
            current_item: Some("分析完成，正在进行审核 AI 复核".to_owned()),
            finished: false,
            cancelled: false,
        }));

        if worker_cancel_flag.load(Ordering::Relaxed) {
            finish_cancelled(&sender, analysis_report);
            return;
        }

        let findings = match request_audit(&config, &candidates, &analyses) {
            Ok(audit) => findings_from_audit(&candidates, &analyses, &audit),
            Err(error) => {
                errors.push(format!("AI 审核失败：{:#}", error));
                audited_fallback_findings(&candidates, &analyses, "AI 审核失败，已转入人工复核。")
            }
        };

        let report = Arc::new(AiAnalysisReport::from_findings(
            root, &config, findings, errors,
        ));
        let _ = sender.send(AiAnalysisEvent::Progress(AiAnalysisProgress {
            report: Arc::clone(&report),
            phase: AiAnalysisPhase::Finished,
            current_item: None,
            finished: true,
            cancelled: false,
        }));
        let _ = sender.send(AiAnalysisEvent::Finished(AiAnalysisFinished {
            report,
            cancelled: false,
        }));
    });

    AiAnalysisHandle {
        receiver,
        cancel_flag,
    }
}

fn collect_candidates(options: &AiAnalysisOptions) -> Vec<CandidateRecord> {
    let limit = options.provider_config.max_candidates.max(1);
    let mut candidates = Vec::new();
    let mut next_id = 1_usize;

    if let Some(preview) = &options.cleanup_preview {
        for candidate in &preview.candidates {
            candidates.push(CandidateRecord {
                id: format!("c{}", next_id),
                path: candidate.path.clone(),
                display_path: candidate.path.display().to_string(),
                size: candidate.size,
                source: AiFindingSource::CleanupPreview,
                category_hint: category_from_cleanup_rule(candidate.rule_id),
                risk_hint: risk_from_cleanup_risk(candidate.risk),
                protected: candidate.protected,
                reason: candidate.reason.clone(),
            });
            next_id += 1;
        }
    }

    if let Some(preview) = &options.duplicate_preview {
        for group in &preview.groups {
            for file in &group.files {
                if file.keep {
                    continue;
                }

                candidates.push(CandidateRecord {
                    id: format!("c{}", next_id),
                    path: file.path.clone(),
                    display_path: file.path.display().to_string(),
                    size: file.size,
                    source: AiFindingSource::DuplicateFile,
                    category_hint: AiCleanupCategory::Duplicate,
                    risk_hint: if file.protected {
                        AiCleanupRisk::High
                    } else {
                        AiCleanupRisk::Low
                    },
                    protected: file.protected,
                    reason: format!(
                        "重复文件 dry-run：与保留文件 {} 哈希相同，当前文件是重复副本。",
                        group.keep_path.display()
                    ),
                });
                next_id += 1;
            }
        }
    }

    candidates.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left.path.cmp(&right.path))
    });
    candidates.truncate(limit);
    candidates
}

fn request_analysis(
    config: &AiProviderConfig,
    candidates: &[CandidateRecord],
) -> anyhow::Result<HashMap<String, AiFindingPayload>> {
    let payloads = candidate_payloads(config, candidates);
    let prompt = format!(
        "请作为磁盘清理分析 AI，对候选文件逐项分类。只输出 JSON，不要 Markdown。\n\
        输出 schema: {{\"findings\":[{{\"id\":\"c1\",\"category\":\"temp_file|cache_file|log_or_dump|backup_or_old|duplicate|installer_or_download|build_artifact|unknown|keep\",\"risk\":\"safe|low|medium|high|unknown\",\"confidence\":0.0,\"recommendation\":\"candidate_for_deletion|keep|needs_review\",\"reason\":\"简短中文理由\"}}]}}。\n\
        规则：遇到 protected=true、系统/程序/用户资料、语义不明或风险不确定时必须 recommendation=needs_review 或 keep；不要因为文件大就建议删除；不要编造未给出的路径信息。path 可能已脱敏。候选：\n{}",
        serde_json::to_string(&payloads)?
    );
    let content = chat_completion(config, analysis_system_prompt(), &prompt)?;
    let envelope: AiFindingsEnvelope = serde_json::from_str(&extract_json_object(&content)?)?;
    Ok(envelope
        .findings
        .into_iter()
        .map(|finding| (finding.id.clone(), finding))
        .collect())
}

fn request_audit(
    config: &AiProviderConfig,
    candidates: &[CandidateRecord],
    analyses: &HashMap<String, AiFindingPayload>,
) -> anyhow::Result<HashMap<String, AuditFindingPayload>> {
    let payloads = candidate_payloads(config, candidates);
    let analysis_payload: Vec<_> = candidates
        .iter()
        .filter_map(|candidate| analyses.get(&candidate.id))
        .collect();
    let prompt = format!(
        "请作为更保守的审核 AI，复核另一个 AI 的磁盘清理判断并纠错，目标是避免误删。只输出 JSON，不要 Markdown。\n\
        输出 schema: {{\"findings\":[{{\"id\":\"c1\",\"audit_status\":\"approved|corrected|rejected|needs_human_review\",\"final_recommendation\":\"candidate_for_deletion|keep|needs_review\",\"category\":\"temp_file|cache_file|log_or_dump|backup_or_old|duplicate|installer_or_download|build_artifact|unknown|keep\",\"risk\":\"safe|low|medium|high|unknown\",\"confidence\":0.0,\"reason\":\"简短中文审核理由\"}}]}}。\n\
        硬性规则：protected=true 不能进入 candidate_for_deletion；高风险/未知风险必须 needs_review 或 keep；只在规则和证据都充分时批准 candidate_for_deletion；可纠正错误分类。候选：\n{}\n分析结果：\n{}",
        serde_json::to_string(&payloads)?,
        serde_json::to_string(&analysis_payload)?
    );
    let content = chat_completion(config, audit_system_prompt(), &prompt)?;
    let envelope: AuditFindingsEnvelope = serde_json::from_str(&extract_json_object(&content)?)?;
    Ok(envelope
        .findings
        .into_iter()
        .map(|finding| (finding.id.clone(), finding))
        .collect())
}

fn chat_completion(
    config: &AiProviderConfig,
    system_prompt: &str,
    user_prompt: &str,
) -> anyhow::Result<String> {
    let api_key = env::var(&config.api_key_env).map_err(|_| {
        anyhow::anyhow!(
            "未找到环境变量 {}。请设置 OpenAI 兼容 API Key；程序不会保存或导出该密钥。",
            config.api_key_env
        )
    })?;
    let endpoint = format!("{}/chat/completions", config.base_url.trim_end_matches('/'));
    let request = ChatRequest {
        model: config.model.clone(),
        messages: vec![
            ChatMessage {
                role: "system".to_owned(),
                content: system_prompt.to_owned(),
            },
            ChatMessage {
                role: "user".to_owned(),
                content: user_prompt.to_owned(),
            },
        ],
        temperature: 0.0,
    };

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(config.timeout_secs.max(5)))
        .build()?;
    let response = client
        .post(endpoint)
        .bearer_auth(api_key)
        .json(&request)
        .send()?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().unwrap_or_default();
        return Err(anyhow::anyhow!(
            "OpenAI 兼容接口返回 HTTP {}：{}",
            status,
            truncate_error_body(&body)
        ));
    }

    let response: ChatResponse = response.json()?;
    response
        .choices
        .into_iter()
        .next()
        .and_then(|choice| {
            let content = choice.message.content.trim().to_owned();
            (!content.is_empty()).then_some(content)
        })
        .ok_or_else(|| anyhow::anyhow!("OpenAI 兼容接口没有返回有效内容"))
}

fn candidate_payloads(
    config: &AiProviderConfig,
    candidates: &[CandidateRecord],
) -> Vec<CandidatePayload> {
    candidates
        .iter()
        .map(|candidate| CandidatePayload {
            id: candidate.id.clone(),
            source: candidate.source.as_str().to_owned(),
            path: if config.send_full_paths {
                candidate.display_path.clone()
            } else {
                redacted_path(candidate)
            },
            size_bytes: candidate.size,
            protected: candidate.protected,
            category_hint: candidate.category_hint.as_str().to_owned(),
            risk_hint: candidate.risk_hint.as_str().to_owned(),
            reason: if config.send_full_paths {
                candidate.reason.clone()
            } else {
                redact_path_like_text(&candidate.reason)
            },
            extension: candidate
                .path
                .extension()
                .and_then(|extension| extension.to_str())
                .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
                .unwrap_or_else(|| "[none]".to_owned()),
            path_depth: candidate.path.components().count(),
        })
        .collect()
}

fn findings_from_analysis(
    candidates: &[CandidateRecord],
    analyses: &HashMap<String, AiFindingPayload>,
) -> Vec<AiReviewFinding> {
    candidates
        .iter()
        .map(|candidate| {
            let analysis = analyses.get(&candidate.id);
            let category = analysis
                .map(|value| AiCleanupCategory::parse(&value.category))
                .filter(|category| *category != AiCleanupCategory::Unknown)
                .unwrap_or(candidate.category_hint);
            let risk = analysis
                .map(|value| AiCleanupRisk::parse(&value.risk))
                .filter(|risk| *risk != AiCleanupRisk::Unknown)
                .unwrap_or(candidate.risk_hint);
            let recommendation = analysis
                .map(|value| AiRecommendation::parse(&value.recommendation))
                .unwrap_or(AiRecommendation::NeedsReview);
            AiReviewFinding {
                path: candidate.path.clone(),
                display_path: candidate.display_path.clone(),
                size: candidate.size,
                source: candidate.source,
                category,
                risk,
                confidence: analysis
                    .map(|value| value.confidence.clamp(0.0, 1.0))
                    .unwrap_or(0.0),
                protected: candidate.protected,
                analysis_recommendation: recommendation,
                analysis_reason: analysis
                    .map(|value| value.reason.clone())
                    .filter(|reason| !reason.trim().is_empty())
                    .unwrap_or_else(|| "AI 未返回该候选的分析结果。".to_owned()),
                audit_status: AiAuditStatus::NeedsHumanReview,
                audit_reason: "等待审核 AI 复核。".to_owned(),
                final_recommendation: AiRecommendation::NeedsReview,
            }
        })
        .collect()
}

fn findings_from_audit(
    candidates: &[CandidateRecord],
    analyses: &HashMap<String, AiFindingPayload>,
    audits: &HashMap<String, AuditFindingPayload>,
) -> Vec<AiReviewFinding> {
    candidates
        .iter()
        .map(|candidate| {
            let analysis = analyses.get(&candidate.id);
            let audit = audits.get(&candidate.id);
            let analysis_category = analysis
                .map(|value| AiCleanupCategory::parse(&value.category))
                .filter(|category| *category != AiCleanupCategory::Unknown)
                .unwrap_or(candidate.category_hint);
            let analysis_risk = analysis
                .map(|value| AiCleanupRisk::parse(&value.risk))
                .filter(|risk| *risk != AiCleanupRisk::Unknown)
                .unwrap_or(candidate.risk_hint);
            let category = audit
                .map(|value| AiCleanupCategory::parse(&value.category))
                .filter(|category| *category != AiCleanupCategory::Unknown)
                .unwrap_or(analysis_category);
            let risk = audit
                .map(|value| AiCleanupRisk::parse(&value.risk))
                .filter(|risk| *risk != AiCleanupRisk::Unknown)
                .unwrap_or(analysis_risk);
            let mut final_recommendation = audit
                .map(|value| AiRecommendation::parse(&value.final_recommendation))
                .unwrap_or(AiRecommendation::NeedsReview);
            let mut audit_status = audit
                .map(|value| AiAuditStatus::parse(&value.audit_status))
                .unwrap_or(AiAuditStatus::NeedsHumanReview);

            if candidate.protected || matches!(risk, AiCleanupRisk::High | AiCleanupRisk::Unknown) {
                if final_recommendation == AiRecommendation::CandidateForDeletion {
                    final_recommendation = AiRecommendation::NeedsReview;
                    audit_status = AiAuditStatus::Corrected;
                }
            }

            AiReviewFinding {
                path: candidate.path.clone(),
                display_path: candidate.display_path.clone(),
                size: candidate.size,
                source: candidate.source,
                category,
                risk,
                confidence: audit
                    .and_then(|value| value.confidence)
                    .or_else(|| analysis.map(|value| value.confidence))
                    .unwrap_or(0.0)
                    .clamp(0.0, 1.0),
                protected: candidate.protected,
                analysis_recommendation: analysis
                    .map(|value| AiRecommendation::parse(&value.recommendation))
                    .unwrap_or(AiRecommendation::NeedsReview),
                analysis_reason: analysis
                    .map(|value| value.reason.clone())
                    .filter(|reason| !reason.trim().is_empty())
                    .unwrap_or_else(|| "AI 未返回该候选的分析结果。".to_owned()),
                audit_status,
                audit_reason: audit
                    .map(|value| value.reason.clone())
                    .filter(|reason| !reason.trim().is_empty())
                    .unwrap_or_else(|| "审核 AI 未返回该候选结果，已转入人工复核。".to_owned()),
                final_recommendation,
            }
        })
        .collect()
}

fn fallback_findings(candidates: &[CandidateRecord], reason: &str) -> Vec<AiReviewFinding> {
    candidates
        .iter()
        .map(|candidate| AiReviewFinding {
            path: candidate.path.clone(),
            display_path: candidate.display_path.clone(),
            size: candidate.size,
            source: candidate.source,
            category: candidate.category_hint,
            risk: AiCleanupRisk::Unknown,
            confidence: 0.0,
            protected: candidate.protected,
            analysis_recommendation: AiRecommendation::NeedsReview,
            analysis_reason: reason.to_owned(),
            audit_status: AiAuditStatus::NeedsHumanReview,
            audit_reason: reason.to_owned(),
            final_recommendation: AiRecommendation::NeedsReview,
        })
        .collect()
}

fn audited_fallback_findings(
    candidates: &[CandidateRecord],
    analyses: &HashMap<String, AiFindingPayload>,
    reason: &str,
) -> Vec<AiReviewFinding> {
    let mut findings = findings_from_analysis(candidates, analyses);
    for finding in &mut findings {
        finding.audit_status = AiAuditStatus::NeedsHumanReview;
        finding.audit_reason = reason.to_owned();
        finding.final_recommendation = AiRecommendation::NeedsReview;
    }
    findings
}

fn finish_cancelled(
    sender: &crossbeam_channel::Sender<AiAnalysisEvent>,
    report: Arc<AiAnalysisReport>,
) {
    let _ = sender.send(AiAnalysisEvent::Progress(AiAnalysisProgress {
        report: Arc::clone(&report),
        phase: AiAnalysisPhase::Finished,
        current_item: None,
        finished: true,
        cancelled: true,
    }));
    let _ = sender.send(AiAnalysisEvent::Finished(AiAnalysisFinished {
        report,
        cancelled: true,
    }));
}

fn category_from_cleanup_rule(rule_id: &str) -> AiCleanupCategory {
    match rule_id {
        "temp_extension" => AiCleanupCategory::TempFile,
        "log_dump_extension" => AiCleanupCategory::LogOrDump,
        "backup_extension" => AiCleanupCategory::BackupOrOld,
        "temp_cache_directory" => AiCleanupCategory::CacheFile,
        _ => AiCleanupCategory::Unknown,
    }
}

fn risk_from_cleanup_risk(risk: CleanupRisk) -> AiCleanupRisk {
    match risk {
        CleanupRisk::Low => AiCleanupRisk::Low,
        CleanupRisk::Medium => AiCleanupRisk::Medium,
    }
}

fn provider_label(config: &AiProviderConfig) -> String {
    let mut host = config.base_url.trim().to_owned();
    if let Some(without_scheme) = host.strip_prefix("https://") {
        host = without_scheme.to_owned();
    } else if let Some(without_scheme) = host.strip_prefix("http://") {
        host = without_scheme.to_owned();
    }
    host.trim_end_matches('/').to_owned()
}

fn redacted_path(candidate: &CandidateRecord) -> String {
    let extension = candidate
        .path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| format!(".{}", extension.to_ascii_lowercase()))
        .unwrap_or_else(|| "[none]".to_owned());
    format!(
        "<redacted:{}:{}:depth{}>",
        candidate.id,
        extension,
        candidate.path.components().count()
    )
}

fn redact_path_like_text(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for token in value.split_whitespace() {
        if token.contains(':') || token.contains('\\') || token.contains('/') {
            result.push_str("<redacted> ");
        } else {
            result.push_str(token);
            result.push(' ');
        }
    }
    result.trim_end().to_owned()
}

fn extract_json_object(content: &str) -> anyhow::Result<String> {
    let trimmed = content.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Ok(trimmed.to_owned());
    }

    let without_fence = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```"))
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim);
    if let Some(value) = without_fence {
        if value.starts_with('{') && value.ends_with('}') {
            return Ok(value.to_owned());
        }
    }

    let start = trimmed.find('{');
    let end = trimmed.rfind('}');
    match (start, end) {
        (Some(start), Some(end)) if start < end => Ok(trimmed[start..=end].to_owned()),
        _ => Err(anyhow::anyhow!("AI 返回内容不是 JSON 对象")),
    }
}

fn normalized_enum(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn truncate_error_body(body: &str) -> String {
    let chars: Vec<_> = body.chars().collect();
    if chars.len() <= 400 {
        return body.to_owned();
    }
    let prefix: String = chars.into_iter().take(400).collect();
    format!("{}…", prefix)
}

fn analysis_system_prompt() -> &'static str {
    "你是谨慎的 Windows/Rust 磁盘空间管理助手。你只能做 dry-run 分析，不能要求删除文件。所有不确定项都应转人工复核。"
}

fn audit_system_prompt() -> &'static str {
    "你是更保守的安全审核 AI，负责纠正磁盘清理分析中的误判，首要目标是避免误删系统、程序、用户资料或证据不足的文件。"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_list_candidate_requires_safe_audit_and_unprotected_path() {
        let mut finding = AiReviewFinding {
            path: PathBuf::from("C:\\tmp\\a.tmp"),
            display_path: "C:\\tmp\\a.tmp".to_owned(),
            size: 10,
            source: AiFindingSource::CleanupPreview,
            category: AiCleanupCategory::TempFile,
            risk: AiCleanupRisk::Low,
            confidence: 0.9,
            protected: false,
            analysis_recommendation: AiRecommendation::CandidateForDeletion,
            analysis_reason: "临时文件".to_owned(),
            audit_status: AiAuditStatus::Approved,
            audit_reason: "通过".to_owned(),
            final_recommendation: AiRecommendation::CandidateForDeletion,
        };

        assert!(finding.is_delete_list_candidate());
        finding.protected = true;
        assert!(!finding.is_delete_list_candidate());
        finding.protected = false;
        finding.risk = AiCleanupRisk::Unknown;
        assert!(!finding.is_delete_list_candidate());
        finding.risk = AiCleanupRisk::Low;
        finding.audit_status = AiAuditStatus::NeedsHumanReview;
        assert!(!finding.is_delete_list_candidate());
    }

    #[test]
    fn extracts_json_from_markdown_fence() {
        let value = extract_json_object("```json\n{\"findings\":[]}\n```").unwrap();
        assert_eq!(value, "{\"findings\":[]}");
    }

    #[test]
    fn redacted_payload_hides_full_path_and_reason_paths() {
        let config = AiProviderConfig::default();
        let candidate = CandidateRecord {
            id: "c1".to_owned(),
            path: PathBuf::from("C:\\Users\\Alice\\secret.tmp"),
            display_path: "C:\\Users\\Alice\\secret.tmp".to_owned(),
            size: 1,
            source: AiFindingSource::CleanupPreview,
            category_hint: AiCleanupCategory::TempFile,
            risk_hint: AiCleanupRisk::Low,
            protected: false,
            reason: "位于 C:\\Users\\Alice\\Temp".to_owned(),
        };
        let payload = candidate_payloads(&config, &[candidate]);
        assert!(payload[0].path.starts_with("<redacted:c1"));
        assert!(!payload[0].reason.contains("Alice"));
    }
}
