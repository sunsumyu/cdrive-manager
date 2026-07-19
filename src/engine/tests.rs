//! 引擎模块测试

#[cfg(test)]
mod classification_tests {
    use crate::engine::classification::*;

    #[test]
    fn system_layer_rank_order() {
        assert!(SystemLayer::Kernel.rank() < SystemLayer::SystemService.rank());
        assert!(SystemLayer::SystemService.rank() < SystemLayer::SystemConfig.rank());
        assert!(SystemLayer::SystemConfig.rank() < SystemLayer::SystemRuntime.rank());
        assert!(SystemLayer::SystemRuntime.rank() < SystemLayer::Application.rank());
        assert!(SystemLayer::Application.rank() < SystemLayer::UserSpace.rank());
    }

    #[test]
    fn file_category_maps_to_correct_layer() {
        // 内核层
        assert!(matches!(FileCategory::SystemBinary.layer(), SystemLayer::Kernel));
        assert!(matches!(FileCategory::ComponentStore.layer(), SystemLayer::Kernel));
        assert!(matches!(FileCategory::SystemDriver.layer(), SystemLayer::Kernel));

        // 服务层
        assert!(matches!(FileCategory::DriverStore.layer(), SystemLayer::SystemService));
        assert!(matches!(FileCategory::ServiceExecutable.layer(), SystemLayer::SystemService));

        // 运行时层
        assert!(matches!(FileCategory::SystemTemp.layer(), SystemLayer::SystemRuntime));
        assert!(matches!(FileCategory::PrefetchData.layer(), SystemLayer::SystemRuntime));
        assert!(matches!(FileCategory::WindowsUpdateCache.layer(), SystemLayer::SystemRuntime));

        // 配置层
        assert!(matches!(FileCategory::WinSxSManifestFile.layer(), SystemLayer::SystemConfig));
        assert!(matches!(FileCategory::GroupPolicy.layer(), SystemLayer::SystemConfig));

        // 应用本体
        assert!(matches!(FileCategory::ApplicationBinary.layer(), SystemLayer::Application));
        assert!(matches!(FileCategory::AppModule.layer(), SystemLayer::Application));

        // 应用数据
        assert!(matches!(FileCategory::BrowserCache.layer(), SystemLayer::Application));
        assert!(matches!(FileCategory::BuildArtifact.layer(), SystemLayer::Application));

        // 用户空间
        assert!(matches!(FileCategory::UserDocument.layer(), SystemLayer::UserSpace));
        assert!(matches!(FileCategory::UserDownload.layer(), SystemLayer::UserSpace));

        // 通用 → 默认用户空间
        assert!(matches!(FileCategory::Unknown.layer(), SystemLayer::UserSpace));
    }

    #[test]
    fn file_importance_base_scores() {
        assert_eq!(FileImportance::Critical.base_score(), 100);
        assert_eq!(FileImportance::Essential.base_score(), 70);
        assert_eq!(FileImportance::Performance.base_score(), 40);
        assert_eq!(FileImportance::Optional.base_score(), 20);
        assert_eq!(FileImportance::Disposable.base_score(), 5);
    }

    #[test]
    fn file_importance_labels() {
        assert_eq!(FileImportance::Critical.label(), "关键");
        assert_eq!(FileImportance::Essential.label(), "重要");
        assert_eq!(FileImportance::Performance.label(), "性能");
        assert_eq!(FileImportance::Optional.label(), "可选");
        assert_eq!(FileImportance::Disposable.label(), "可丢弃");
    }

    #[test]
    fn file_classification_builder() {
        let fc = FileClassification::new(
            "C:\\Windows\\Temp\\test.tmp",
            FileCategory::SystemTemp,
            FileImportance::Disposable,
        )
        .with_owner("Windows")
        .with_role("系统临时文件");

        assert_eq!(fc.path, std::path::PathBuf::from("C:\\Windows\\Temp\\test.tmp"));
        assert!(matches!(fc.layer, SystemLayer::SystemRuntime));
        assert!(matches!(fc.category, FileCategory::SystemTemp));
        assert_eq!(fc.owner, Some("Windows".to_string()));
        assert_eq!(fc.role_in_app, Some("系统临时文件".to_string()));
        assert!(matches!(fc.importance, FileImportance::Disposable));
    }
}

#[cfg(test)]
mod risk_assessment_tests {
    use crate::engine::risk_assessment::*;

    #[test]
    fn risk_level_from_score() {
        assert!(matches!(RiskLevel::from_score(0), RiskLevel::Safe));
        assert!(matches!(RiskLevel::from_score(5), RiskLevel::Safe));
        assert!(matches!(RiskLevel::from_score(10), RiskLevel::Safe));
        assert!(matches!(RiskLevel::from_score(11), RiskLevel::Low));
        assert!(matches!(RiskLevel::from_score(30), RiskLevel::Low));
        assert!(matches!(RiskLevel::from_score(31), RiskLevel::Medium));
        assert!(matches!(RiskLevel::from_score(60), RiskLevel::Medium));
        assert!(matches!(RiskLevel::from_score(61), RiskLevel::High));
        assert!(matches!(RiskLevel::from_score(90), RiskLevel::High));
        assert!(matches!(RiskLevel::from_score(91), RiskLevel::Critical));
        assert!(matches!(RiskLevel::from_score(100), RiskLevel::Critical));
    }

    #[test]
    fn risk_assessment_from_base_score() {
        let assessment = RiskAssessment::from_base(40);
        assert_eq!(assessment.base_score, 40);
        assert_eq!(assessment.final_score, 40);
        assert!(matches!(assessment.level, RiskLevel::Medium));
        assert!(!assessment.is_safe_to_delete()); // 40 > 30
    }

    #[test]
    fn risk_assessment_with_adjustments() {
        let assessment = RiskAssessment::from_base(20)
            .with_dynamic(15)     // +15
            .with_age(-10)        // -10
            .with_dependency(5);  // +5

        // 20 + 15 - 10 + 5 = 30
        assert_eq!(assessment.final_score, 30);
        assert!(matches!(assessment.level, RiskLevel::Low));
        assert!(assessment.is_safe_to_delete()); // 30 <= 30
    }

    #[test]
    fn risk_assessment_clamps_to_0_100() {
        // 超过 100 应被钳制
        let high = RiskAssessment::from_base(90)
            .with_dynamic(20);  // 90 + 20 = 110 → 100
        assert_eq!(high.final_score, 100);
        assert!(matches!(high.level, RiskLevel::Critical));

        // 低于 0 应被钳制
        let low = RiskAssessment::from_base(10)
            .with_dynamic(-30);  // 10 - 30 = -20 → 0
        assert_eq!(low.final_score, 0);
        assert!(matches!(low.level, RiskLevel::Safe));
    }

    #[test]
    fn recommendation_labels() {
        assert_eq!(Recommendation::Delete.label(), "删除");
        assert_eq!(Recommendation::Keep.label(), "保留");
        assert_eq!(Recommendation::Review.label(), "需确认");
        assert_eq!(Recommendation::Unknown.label(), "未知");
    }

    #[test]
    fn decision_source_labels() {
        assert_eq!(DecisionSource::StaticRule.label(), "静态规则");
        assert_eq!(DecisionSource::DynamicPerception.label(), "动态感知");
        assert_eq!(DecisionSource::AiAssist.label(), "AI 辅助");
        assert_eq!(DecisionSource::UserConfirmed.label(), "用户确认");
        assert_eq!(DecisionSource::Unknown.label(), "未知");
    }
}

#[cfg(test)]
mod static_rules_tests {
    use crate::engine::static_rules::*;
    use std::path::Path;

    #[test]
    fn engine_classifies_system_binary() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Windows\\System32\\kernel32.dll"));
        assert!(result.is_some(), "System32 kernel32.dll should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.layer, crate::engine::SystemLayer::Kernel));
        assert!(matches!(result.classification.category, crate::engine::FileCategory::SystemBinary));
    }

    #[test]
    fn engine_classifies_winsxs() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Windows\\WinSxS\\amd64_microsoft-windows-shell32_31bf3856ad364e35_10.0.19041.1_none_7c8c5c5c5c5c5c5c\\shell32.dll"));
        assert!(result.is_some(), "WinSxS file should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.layer, crate::engine::SystemLayer::Kernel));
        assert!(matches!(result.classification.category, crate::engine::FileCategory::ComponentStore));
    }

    #[test]
    fn engine_classifies_windows_update_cache() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Windows\\SoftwareDistribution\\Download\\KB123456\\payload.cab"));
        assert!(result.is_some(), "Windows Update cache should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::WindowsUpdateCache));
    }

    #[test]
    fn engine_classifies_prefetch() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Windows\\Prefetch\\CHROME.EXE-12345678.pf"));
        assert!(result.is_some(), "Prefetch file should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::PrefetchData));
    }

    #[test]
    fn engine_classifies_wer_reports() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\ProgramData\\Microsoft\\Windows\\WER\\ReportQueue\\AppCrash_guid\\report.wer"));
        assert!(result.is_some(), "WER report should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::ErrorReport));
    }

    #[test]
    fn engine_classifies_windows_temp() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Windows\\Temp\\foo.tmp"));
        assert!(result.is_some(), "Windows Temp should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::SystemTemp));
    }

    #[test]
    fn engine_classifies_windows_old() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Windows.old\\Windows\\System32\\drivers\\etc\\hosts"));
        assert!(result.is_some(), "Windows.old should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.layer, crate::engine::SystemLayer::SystemRuntime));
    }

    #[test]
    fn engine_classifies_browser_cache() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Users\\Alice\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Cache\\data_1"));
        assert!(result.is_some(), "Chrome cache should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::BrowserCache));
    }

    #[test]
    fn engine_classifies_package_cache_npm() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Users\\Alice\\.npm\\_cacache\\index-v5\\xx\\yy"));
        assert!(result.is_some(), "npm cache should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::PackageManagerCache));
    }

    #[test]
    fn engine_classifies_build_artifact() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\dev\\myapp\\target\\debug\\myapp.exe"));
        assert!(result.is_some(), "Build artifact should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::BuildArtifact));
    }

    #[test]
    fn engine_classifies_thumbnail_cache() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Users\\Alice\\AppData\\Local\\Microsoft\\Windows\\Explorer\\thumbcache_256.db"));
        assert!(result.is_some(), "Thumbnail cache should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::ThumbnailCache));
    }

    #[test]
    fn engine_classifies_temp_extension() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\work\\scratch\\x.tmp"));
        assert!(result.is_some(), ".tmp should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::GenericTemp));
    }

    #[test]
    fn engine_classifies_log_extension() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\work\\app.log"));
        assert!(result.is_some(), ".log should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::GenericLog));
    }

    #[test]
    fn engine_classifies_download_fragment() {
        let engine = StaticRuleEngine::new();
        let result = engine.classify(Path::new("C:\\Users\\Alice\\Downloads\\video.crdownload"));
        assert!(result.is_some(), ".crdownload should be classified");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::GenericFragment));
    }

    #[test]
    fn engine_does_not_classify_unknown() {
        let engine = StaticRuleEngine::new();
        // 使用一个真正未知的路径（不在任何规则中）
        let result = engine.classify(Path::new("C:\\MyCustomApp\\data.dat"));
        assert!(result.is_none(), "Unknown path should not be classified by static rules");
    }

    // ─── 受保护路径测试 ───

    #[test]
    fn protected_system32() {
        let engine = StaticRuleEngine::new();
        assert!(engine.is_protected_path(Path::new("C:\\Windows\\System32\\kernel32.dll")));
        assert!(engine.is_protected_path(Path::new("C:\\Windows\\System32")));
    }

    #[test]
    fn protected_winsxs() {
        let engine = StaticRuleEngine::new();
        assert!(engine.is_protected_path(Path::new("C:\\Windows\\WinSxS\\manifest")));
    }

    #[test]
    fn protected_installer() {
        let engine = StaticRuleEngine::new();
        assert!(engine.is_protected_path(Path::new("C:\\Windows\\Installer\\abc.msi")));
    }

    #[test]
    fn protected_program_files() {
        let engine = StaticRuleEngine::new();
        assert!(engine.is_protected_path(Path::new("C:\\Program Files\\App\\app.exe")));
        assert!(engine.is_protected_path(Path::new("C:\\Program Files (x86)\\App\\app.exe")));
    }

    #[test]
    fn protected_programdata() {
        let engine = StaticRuleEngine::new();
        assert!(engine.is_protected_path(Path::new("C:\\ProgramData\\App\\state.db")));
    }

    #[test]
    fn protected_user_libraries() {
        let engine = StaticRuleEngine::new();
        assert!(engine.is_protected_path(Path::new("C:\\Users\\Alice\\Desktop\\file.txt")));
        assert!(engine.is_protected_path(Path::new("C:\\Users\\Alice\\Downloads\\setup.exe")));
        assert!(engine.is_protected_path(Path::new("C:\\Users\\Bob\\文档\\report.docx")));
    }

    #[test]
    fn not_protected_outside_users() {
        let engine = StaticRuleEngine::new();
        assert!(!engine.is_protected_path(Path::new("C:\\work\\Downloads\\setup.exe")));
        assert!(!engine.is_protected_path(Path::new("D:\\backup\\Documents\\x")));
    }

    #[test]
    fn not_protected_windows_old() {
        let engine = StaticRuleEngine::new();
        assert!(!engine.is_protected_path(Path::new("C:\\Windows.old\\Windows\\System32")));
    }

    #[test]
    fn not_protected_user_document() {
        let engine = StaticRuleEngine::new();
        // Users 目录下的 Documents 是受保护的库文件夹，但不在 Users 下的同名文件夹不受保护
        assert!(engine.is_protected_path(Path::new("C:\\Users\\Alice\\Documents\\report.docx")));
        assert!(!engine.is_protected_path(Path::new("C:\\work\\Documents\\report.docx")));
    }
}

#[cfg(test)]
mod risk_engine_tests {
    use crate::engine::risk_engine::{RiskEngine, SafetyVerdict};
    use std::path::Path;

    #[test]
    fn risk_engine_analyzes_system_temp() {
        let engine = RiskEngine::new();
        let result = engine.analyze(Path::new("C:\\Windows\\Temp\\foo.tmp"));
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.risk.is_safe_to_delete());
        assert_eq!(result.impact_description, "可安全删除，下次需要时自动重建");
    }

    #[test]
    fn risk_engine_analyzes_browser_cache() {
        let engine = RiskEngine::new();
        let result = engine.analyze(Path::new("C:\\Users\\Alice\\AppData\\Local\\Google\\Chrome\\User Data\\Default\\Cache\\data_1"));
        assert!(result.is_some());
        let result = result.unwrap();
        assert!(result.risk.is_safe_to_delete());
    }

    #[test]
    fn risk_engine_analyzes_unknown() {
        let engine = RiskEngine::new();
        let result = engine.analyze(Path::new("C:\\Users\\Alice\\Documents\\report.docx"));
        // 未知文件通过 AI 返回 Unknown 分类（保守策略）
        assert!(result.is_some(), "AI should return Unknown classification for unknown files");
        let result = result.unwrap();
        assert!(matches!(result.classification.category, crate::engine::FileCategory::Unknown));
        // 风险分应为 70（Essential）
        assert_eq!(result.risk.final_score, 70);
    }

    #[test]
    fn risk_engine_can_delete_safe_file() {
        let engine = RiskEngine::new();
        // Windows Update 下载缓存是可安全删除的（不在系统保护路径中）
        let verdict = engine.can_delete(Path::new("C:\\Windows\\SoftwareDistribution\\Download\\KB12345\\payload.cab"));
        assert!(verdict.is_safe(), "Windows Update cache should be safe to delete");
    }

    #[test]
    fn risk_engine_cannot_delete_system_file() {
        let engine = RiskEngine::new();
        let verdict = engine.can_delete(Path::new("C:\\Windows\\System32\\kernel32.dll"));
        assert!(!verdict.is_safe());
        assert!(verdict.is_unsafe());
    }

    #[test]
    fn risk_engine_cannot_delete_protected_path() {
        let engine = RiskEngine::new();
        let verdict = engine.can_delete(Path::new("C:\\ProgramData\\App\\state.db"));
        assert!(!verdict.is_safe());
    }

    #[test]
    fn safety_verdict_states() {
        assert!(SafetyVerdict::Safe.is_safe());
        assert!(!SafetyVerdict::Safe.is_unsafe());
        assert!(!SafetyVerdict::Safe.is_unknown());

        assert!(!SafetyVerdict::Unsafe("test".to_string()).is_safe());
        assert!(SafetyVerdict::Unsafe("test".to_string()).is_unsafe());
        assert!(!SafetyVerdict::Unsafe("test".to_string()).is_unknown());

        assert!(!SafetyVerdict::Unknown.is_safe());
        assert!(!SafetyVerdict::Unknown.is_unsafe());
        assert!(SafetyVerdict::Unknown.is_unknown());
    }
}

#[cfg(test)]
mod glob_tests {
    use crate::engine::static_rules::*;

    #[test]
    fn glob_matches_star() {
        assert!(glob_matches("thumbcache_256.db", "thumbcache_*.db"));
        assert!(glob_matches("iconcache_128.db", "iconcache_*.db"));
        assert!(!glob_matches("foo.txt", "thumbcache_*.db"));
    }

    #[test]
    fn glob_matches_exact() {
        assert!(glob_matches("anything", "*"));
        assert!(glob_matches("abc.txt", "abc.txt"));
    }

    #[test]
    fn glob_matches_prefix_suffix() {
        assert!(glob_matches("test_file_name.ext", "test_*.ext"));
        assert!(!glob_matches("test_file_name.ext", "test_*.txt"));
    }

    #[test]
    fn path_prefix_component_boundary() {
        assert!(path_matches_prefix("c:\\windows\\temp", "c:\\windows"));
        assert!(!path_matches_prefix("c:\\windows.old", "c:\\windows"));
        assert!(path_matches_prefix("c:\\windows", "c:\\windows"));
        assert!(!path_matches_prefix("c:\\windows.old\\temp", "c:\\windows"));
    }
}

#[cfg(test)]
mod knowledge_base_tests {
    use std::path::Path;
    use std::sync::Arc;

    use crate::engine::classification::{FileCategory, FileImportance};
    use crate::engine::knowledge_base::KnowledgeBase;
    use crate::engine::risk_assessment::RiskAssessment;
    use crate::engine::risk_engine::{AnalysisResult, RiskEngine};

    #[test]
    fn knowledge_base_open_in_memory() {
        let kb = KnowledgeBase::open_in_memory();
        assert!(kb.is_ok());
    }

    #[test]
    fn knowledge_base_put_and_get() {
        let kb = KnowledgeBase::open_in_memory().unwrap();
        let path = Path::new("C:\\Windows\\Temp\\test_cache.tmp");

        // 构造一个 AnalysisResult
        let result = AnalysisResult {
            classification: crate::engine::FileClassification::new(
                path,
                FileCategory::SystemTemp,
                FileImportance::Disposable,
            ),
            risk: RiskAssessment::from_base(5),
            impact_description: "测试影响".to_string(),
            dynamic_snapshot: None,
        };

        // 写入缓存
        kb.put(path, 12345, &result).unwrap();

        // 读取缓存（相同 modified）
        let cached = kb.get(path, 12345);
        assert!(cached.is_some());
        let cached = cached.unwrap();
        assert!(matches!(cached.classification.category, FileCategory::SystemTemp));
        assert_eq!(cached.risk.base_score, 5);
    }

    #[test]
    fn knowledge_base_miss_on_different_modified() {
        let kb = KnowledgeBase::open_in_memory().unwrap();
        let path = Path::new("C:\\Windows\\Temp\\test_cache.tmp");

        let result = AnalysisResult {
            classification: crate::engine::FileClassification::new(
                path,
                FileCategory::SystemTemp,
                FileImportance::Disposable,
            ),
            risk: RiskAssessment::from_base(5),
            impact_description: "测试影响".to_string(),
            dynamic_snapshot: None,
        };

        kb.put(path, 12345, &result).unwrap();

        // 不同 modified → 缓存未命中
        let cached = kb.get(path, 99999);
        assert!(cached.is_none());
    }

    #[test]
    fn knowledge_base_entry_count() {
        let kb = KnowledgeBase::open_in_memory().unwrap();
        assert_eq!(kb.entry_count().unwrap(), 0);

        let path = Path::new("C:\\test\\a.tmp");
        let result = AnalysisResult {
            classification: crate::engine::FileClassification::new(path, FileCategory::GenericTemp, FileImportance::Disposable),
            risk: RiskAssessment::from_base(5),
            impact_description: "test".to_string(),
            dynamic_snapshot: None,
        };
        kb.put(path, 1, &result).unwrap();
        assert_eq!(kb.entry_count().unwrap(), 1);
    }

    #[test]
    fn risk_engine_with_kb_uses_cache() {
        let kb = Arc::new(KnowledgeBase::open_in_memory().unwrap());
        let engine = RiskEngine::with_kb(kb.clone());

        // 使用实际存在的临时文件路径以便缓存读写
        let temp_file = std::env::temp_dir().join("cdrive_test_cache.tmp");
        std::fs::write(&temp_file, b"test").unwrap();
        let path = temp_file.as_path();

        // 第一次分析（不命中缓存，走静态规则）
        let result1 = engine.analyze(path);
        assert!(result1.is_some());

        // 验证知识库已写入条目
        assert!(kb.entry_count().unwrap() >= 1);

        // 第二次分析（命中缓存）
        let result2 = engine.analyze(path);
        assert!(result2.is_some());
        assert_eq!(result1.unwrap().risk.base_score, result2.unwrap().risk.base_score);

        // 清理
        let _ = std::fs::remove_file(&temp_file);
    }

    #[test]
    fn knowledge_base_gc_and_clear() {
        let kb = KnowledgeBase::open_in_memory().unwrap();
        let path = Path::new("C:\\test\\old.tmp");

        let result = AnalysisResult {
            classification: crate::engine::FileClassification::new(path, FileCategory::GenericTemp, FileImportance::Disposable),
            risk: RiskAssessment::from_base(5),
            impact_description: "old".to_string(),
            dynamic_snapshot: None,
        };

        kb.put(path, 1, &result).unwrap();
        assert_eq!(kb.entry_count().unwrap(), 1);

        // clear_all 应清空所有条目
        let cleared = kb.clear_all().unwrap();
        assert_eq!(cleared, 1);
        assert_eq!(kb.entry_count().unwrap(), 0);
    }
}
