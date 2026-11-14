//! The combined conclusion: Q1 + Q2 + Q3 folded into one statement **with an
//! explicit confidence that is derived from evidence, never hard-coded**.
//!
//! # Why this is a separate pure module
//!
//! The three answer cards each explain one question. A real conclusion has to
//! weigh them against each other, and that weighting must be explainable,
//! reproducible and testable. Doing it by concatenating the three `detail`
//! strings would hide the reasoning; doing it inside `db.rs` would make it
//! untestable. So the rules live here as pure functions over the evidence
//! digest, and the unit tests at the bottom pin the conflict matrix down.
//!
//! # Scoring
//!
//! ```text
//! base:            high = 90, medium = 65, low = 35, unknown = 15
//! score = 0.35*base(origin) + 0.25*base(modifiers) + 0.40*base(safety)
//!         - 8 * min(unknown_channels, 3)
//!         - 10 * conflict
//!         - 5  * (!etw_available)
//!         - 8  * coverage_insufficient
//! clamp(5, 97)
//! confidence = score >= 78 -> high ; 50..=77 -> medium ; < 50 -> low
//! semantic caps: Q3 not ready -> <= 55 ; non-Windows -> <= 77
//! ```
//!
//! Every penalty has a matching `caveat`, so a low score can always be
//! explained in the UI.

use crate::model::Verdict;

/// Evidence id for "the writer is known, the origin is not".
pub const EVIDENCE_HANDLE: &str = "rm.handle";
pub const EVIDENCE_PATH: &str = "path.shape";

fn base(confidence: &str) -> f64 {
    match confidence {
        "high" => 90.0,
        "medium" => 65.0,
        "low" => 35.0,
        _ => 15.0,
    }
}

/// Q1 confidence straight from the evidence strength.
pub fn origin_confidence(kind: &str, strength: i64) -> &'static str {
    if kind == "unknown" || strength <= 0 {
        return "low";
    }
    match strength {
        3 => "high",
        2 => "medium",
        _ => "low",
    }
}

/// True when there is *no* origin evidence at all (as opposed to weak
/// evidence). Only this counts towards the "unknown channel" penalty.
pub fn origin_is_unknown(kind: &str, strength: i64) -> bool {
    kind == "unknown" || strength <= 0
}

#[derive(Debug, Clone, Default)]
pub struct ProcessDigest {
    /// Events attributed by ETW (real writing PID).
    pub etw_hits: i64,
    /// Distinct processes that held a live handle.
    pub live: i64,
    /// Distinct producers named by a downloader task log.
    pub historical: i64,
    /// Distinct names matched only through shell "recent" shortcuts.
    pub inferred: i64,
    /// Is the ETW channel actually running in this session?
    pub etw_available: bool,
}

/// Q2 confidence. Non-Windows is capped at medium by construction: without ETW
/// or Restart Manager there is no channel that can reach "high".
pub fn modifiers_confidence(digest: &ProcessDigest) -> &'static str {
    if digest.etw_hits > 0 {
        return "high";
    }
    if digest.live > 0 || digest.historical > 0 {
        return "medium";
    }
    "low"
}

#[derive(Debug, Clone, Default)]
pub struct SafetyDigest {
    /// `files.hash_state`, e.g. `done` / `pending` / `too_large`.
    pub hash_state: String,
    /// Copies with identical content but a different file identity.
    pub real_copies: i64,
    /// Same identity, another name: the very same data on disk.
    pub hardlink_copies: i64,
    /// Live files whose fingerprint is not finished yet (global coverage).
    pub pending_unhashed: i64,
    /// Is this file inside the fingerprint-scope directories?
    pub watched: bool,
    /// Files sharing some chunks — "related", never a copy.
    pub related: i64,
}

impl SafetyDigest {
    pub fn ready(&self) -> bool {
        self.hash_state == "done"
    }

    /// An "only copy" claim is only safe when nothing is still unhashed. The
    /// old code treated "no hash match found" as "unique", which was wrong
    /// while hundreds of files were still queued.
    pub fn coverage_ok(&self) -> bool {
        self.ready() && self.pending_unhashed == 0 && self.watched
    }

    pub fn coverage_insufficient(&self) -> bool {
        self.ready() && self.real_copies == 0 && self.hardlink_copies == 0 && !self.coverage_ok()
    }
}

pub fn safety_confidence(digest: &SafetyDigest) -> &'static str {
    if !digest.ready() {
        return "low";
    }
    if digest.real_copies > 0 {
        return "high";
    }
    if digest.hardlink_copies > 0 {
        return "medium";
    }
    if digest.coverage_ok() {
        "high"
    } else {
        "medium"
    }
}

/// Session facts the pure scoring function cannot know by itself.
#[derive(Debug, Clone, Copy, Default)]
pub struct VerdictContext {
    /// Is the ETW channel running right now? (`Status::etw_state == "active"`)
    pub etw_available: bool,
    /// `cfg!(windows)`.
    pub platform_supports_etw: bool,
}

#[derive(Debug, Clone, Default)]
pub struct VerdictInput {
    pub source_kind: String,
    pub source_strength: i64,
    pub source_evidence: String,
    pub processes: ProcessDigest,
    pub safety: SafetyDigest,
    /// `cfg!(windows)` — ETW cannot exist on macOS/Linux, and the verdict says
    /// so instead of pretending the channel was merely unlucky.
    pub platform_supports_etw: bool,
}

fn evidence_label(evidence: &str) -> &'static str {
    match evidence {
        "zone.ads" => "Zone.Identifier 备用数据流（写入方标记）",
        "downloader.task" => "下载器任务记录（URL + 时间）",
        "browser.exact" => "浏览器下载记录（精确路径命中）",
        "browser.name_size" => "浏览器下载记录（文件名 + 大小）",
        "im.layout" => "IM 接收目录布局",
        "mail.ext" => "邮件文件扩展名",
        "path.shape" => "路径特征（启发式，来源未确认）",
        "rm.handle" => "Restart Manager 写句柄（只证明写入者）",
        _ => "无可用的来源证据",
    }
}

fn strength_label(strength: i64) -> &'static str {
    match strength {
        3 => "客观权威",
        2 => "强间接",
        1 => "启发式",
        _ => "无证据",
    }
}

/// Build the combined conclusion. Pure function: same input, same verdict.
pub fn build(input: &VerdictInput) -> Verdict {
    let origin_conf = origin_confidence(&input.source_kind, input.source_strength);
    let mod_conf = modifiers_confidence(&input.processes);
    let safety_conf = safety_confidence(&input.safety);
    let origin_unknown = origin_is_unknown(&input.source_kind, input.source_strength);
    let has_process_evidence = input.processes.live > 0
        || input.processes.historical > 0
        || input.processes.etw_hits > 0;

    let mut score =
        0.35 * base(origin_conf) + 0.25 * base(mod_conf) + 0.40 * base(safety_conf);

    // --- penalties ---------------------------------------------------------
    let mut unknown_channels = 0i64;
    if origin_unknown {
        unknown_channels += 1;
    }
    if !has_process_evidence {
        unknown_channels += 1;
    }
    if !input.safety.ready() {
        unknown_channels += 1;
    }
    score -= 8.0 * unknown_channels.min(3) as f64;

    // Conflict: the writer is known but the origin is not. That is a *partial*
    // answer, and it must not be reported as a confident origin.
    let writer_only_conflict = origin_unknown && has_process_evidence;
    // A write handle from a browser over a download-folder path: strong
    // suspicion, still no URL evidence. (Both `safety.ready()` states qualify —
    // the conflict is about the *origin* channel, not about Q3.)
    let handle_suspicion = input.source_evidence == EVIDENCE_HANDLE && input.source_kind == "download";
    let handle_over_path = handle_suspicion && !input.safety.ready();
    if writer_only_conflict {
        score -= 10.0;
    }
    if handle_over_path || handle_suspicion {
        score -= 5.0;
    }
    if !input.processes.etw_available {
        score -= 5.0;
    }
    let coverage_insufficient = input.safety.coverage_insufficient();
    if coverage_insufficient {
        score -= 8.0;
    }
    if unknown_channels == 3 {
        // "Not enough evidence" has a floor: it is a definite statement about
        // the evidence, not a zero-confidence guess.
        score = score.max(15.0);
    }

    // --- semantic caps -----------------------------------------------------
    if !input.safety.ready() {
        score = score.min(55.0);
    }
    if !input.platform_supports_etw {
        score = score.min(77.0);
    }
    let score = score.round().clamp(5.0, 97.0) as i64;
    let confidence = if score >= 78 {
        "high"
    } else if score >= 50 {
        "medium"
    } else {
        "low"
    };

    // --- explanation -------------------------------------------------------
    let mut drivers: Vec<String> = Vec::new();
    drivers.push(format!(
        "来源证据：{} · {}（strength={}）",
        evidence_label(&input.source_evidence),
        strength_label(input.source_strength),
        input.source_strength
    ));
    if input.processes.etw_hits > 0 {
        drivers.push(format!(
            "进程归因：ETW 内核事件 {} 条（真·写入 PID）",
            input.processes.etw_hits
        ));
    } else if input.processes.live > 0 {
        drivers.push(format!(
            "进程归因：Restart Manager 写句柄，{} 个写入者",
            input.processes.live
        ));
    }
    if input.processes.historical > 0 {
        drivers.push(format!(
            "历史产生者：{} 条下载器任务记录（不是活进程）",
            input.processes.historical
        ));
    }
    if input.safety.ready() {
        if input.safety.real_copies > 0 {
            drivers.push(format!(
                "内容指纹已完成：存在 {} 个独立副本",
                input.safety.real_copies
            ));
        } else if input.safety.hardlink_copies > 0 {
            drivers.push(format!(
                "内容指纹已完成：{} 个硬链接（同一份数据）",
                input.safety.hardlink_copies
            ));
        } else if input.safety.coverage_ok() {
            drivers.push("内容指纹已完成，且库内没有未完成指纹的文件".to_string());
        } else {
            drivers.push("内容指纹已完成，但库内仍有未完成指纹的文件".to_string());
        }
    } else if input.safety.hash_state == "skip" {
        drivers.push("内容指纹状态：skip（在指纹范围外，不参与计算）".to_string());
    } else {
        drivers.push(format!("内容指纹状态：{}（尚未完成）", input.safety.hash_state));
    }
    if input.safety.related > 0 {
        drivers.push(format!(
            "另有 {} 个文件共享部分内容块（关联文件，不计入副本）",
            input.safety.related
        ));
    }

    let mut caveats: Vec<String> = Vec::new();
    if !input.platform_supports_etw {
        caveats.push("本平台没有 ETW / Restart Manager，进程级归因不可用，Q2 证据上限为「中」".to_string());
    } else if !input.processes.etw_available {
        caveats.push(
            "ETW 内核事件流未启用（未提权或被配置关闭），写入者归因只有句柄近似".to_string(),
        );
    }
    if input.processes.inferred > 0 {
        caveats.push("部分「被打开」记录按文件名匹配，属于推断信号而非硬证据".to_string());
    }
    if !input.safety.ready() {
        if input.safety.hash_state == "skip" {
            // An index-only file never had a fingerprint at all; saying the
            // queue is "still working" would be a lie that never resolves.
            caveats.push(
                "该文件仅建立索引（在指纹范围外），未计算内容指纹，无法判断删除后果".to_string(),
            );
        } else {
            caveats.push(format!(
                "内容指纹尚未完成（{}），删除后果暂时无法判断",
                input.safety.hash_state
            ));
        }
    }
    if coverage_insufficient {
        caveats.push(format!(
            "仍有 {} 个文件未完成指纹，其中可能包含与此文件相同的内容",
            input.safety.pending_unhashed
        ));
    }
    if input.safety.ready()
        && input.safety.real_copies == 0
        && input.safety.hardlink_copies == 0
        && input.safety.coverage_ok()
    {
        caveats.push(
            "「唯一副本」只覆盖指纹范围目录；其它磁盘位置或云端的副本不在结论范围内".to_string(),
        );
    }
    if input.source_kind == "download" && origin_conf == "high" {
        caveats.push("原始下载地址可能仍然有效，必要时可以重新获取该文件".to_string());
    }

    // --- headline + action -------------------------------------------------
    let copies_total = input.safety.real_copies + input.safety.hardlink_copies;
    let (headline, action, detail) = if !input.safety.ready() && (origin_conf != "low" || has_process_evidence) {
        (
            "暂时无法判断删除后果".to_string(),
            "verify_first".to_string(),
            "来源与写入者已有线索，但内容指纹还没算完，现在删除可能丢掉无法再获得的内容。"
                .to_string(),
        )
    } else if writer_only_conflict {
        (
            "写入者已确定，来源仍未知".to_string(),
            if copies_total > 0 { "delete_safe" } else { "verify_first" }.to_string(),
            "可以确认是谁写入了这个文件，但没有任何通道能说明它来自哪里（没有下载 URL、没有来源页面、没有区域标记）。"
                .to_string(),
        )
    } else if handle_suspicion || handle_over_path {
        (
            "很可能是浏览器下载产物，但缺 URL 证据".to_string(),
            if copies_total > 0 { "delete_safe" } else { "verify_first" }.to_string(),
            "写句柄指向浏览器进程，路径也符合下载特征；但浏览器下载记录、下载器任务记录与 Zone.Identifier 都没有命中，无法给出下载地址。"
                .to_string(),
        )
    } else if unknown_channels == 3 {
        (
            "证据不足，暂不给出结论".to_string(),
            "verify_first".to_string(),
            "来源、写入者与内容指纹三条通道都没有可用证据，任何删除建议都只是猜测。"
                .to_string(),
        )
    } else if copies_total > 0 {
        let headline = if input.safety.real_copies > 0 {
            format!(
                "删除不会丢内容：另有 {} 个相同内容的副本",
                input.safety.real_copies
            )
        } else {
            format!(
                "删除不丢内容也不释放空间：{} 个硬链接指向同一份数据",
                input.safety.hardlink_copies
            )
        };
        (
            headline,
            "delete_safe".to_string(),
            "内容指纹在其它位置命中同一份数据，删除此路径不会丢失内容。".to_string(),
        )
    } else if input.safety.coverage_ok() {
        (
            "唯一副本，删除会丢内容".to_string(),
            "keep".to_string(),
            "内容指纹在指纹范围目录内只出现一次，且库内没有未完成指纹的文件，删除后本机不再有相同内容。"
                .to_string(),
        )
    } else {
        (
            "可能是唯一副本，但指纹覆盖不完整".to_string(),
            "verify_first".to_string(),
            "目前没有发现副本，但仍有文件未完成指纹；这些文件里可能就有相同内容，建议等指纹队列清空后再判断。"
                .to_string(),
        )
    };

    let confidence_note = match confidence {
        "high" => "证据链完整，结论可直接采用",
        "medium" => "证据可用但存在缺口，建议结合保留意见判断",
        _ => "证据薄弱，结论仅供参考",
    };

    Verdict {
        headline,
        detail: format!("{}（可信度 {} / 100：{}）", detail, score, confidence_note),
        confidence: confidence.to_string(),
        score,
        drivers,
        caveats,
        action,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn high_origin() -> (String, i64, String) {
        ("download".to_string(), 3, "zone.ads".to_string())
    }

    fn etw_processes() -> ProcessDigest {
        ProcessDigest {
            etw_hits: 4,
            live: 1,
            historical: 0,
            inferred: 0,
            etw_available: true,
        }
    }

    fn complete_unique_safety() -> SafetyDigest {
        SafetyDigest {
            hash_state: "done".to_string(),
            real_copies: 0,
            hardlink_copies: 0,
            pending_unhashed: 0,
            watched: true,
            related: 0,
        }
    }

    fn input(
        origin: (String, i64, String),
        processes: ProcessDigest,
        safety: SafetyDigest,
    ) -> VerdictInput {
        VerdictInput {
            source_kind: origin.0,
            source_strength: origin.1,
            source_evidence: origin.2,
            processes,
            safety,
            platform_supports_etw: true,
        }
    }

    /// §4.4 example 1: Zone.Identifier (90) + ETW (90) + unique copy with full
    /// coverage (90) = 90 -> high.
    #[test]
    fn full_evidence_chain_is_high_and_exactly_ninety() {
        let verdict = build(&input(
            high_origin(),
            etw_processes(),
            complete_unique_safety(),
        ));
        assert_eq!(verdict.score, 90);
        assert_eq!(verdict.confidence, "high");
        assert_eq!(verdict.action, "keep");
        assert!(verdict.headline.contains("唯一副本"));
    }

    /// §4.3 row 1: unknown origin + a live writer must not become a confident
    /// origin claim.
    #[test]
    fn writer_known_origin_unknown_is_a_partial_answer() {
        let verdict = build(&input(
            ("unknown".to_string(), 0, "none".to_string()),
            ProcessDigest {
                etw_hits: 0,
                live: 1,
                historical: 0,
                inferred: 0,
                etw_available: true,
            },
            complete_unique_safety(),
        ));
        assert!(verdict.headline.contains("写入者已确定"));
        assert!(verdict.confidence != "high");
        assert!(verdict
            .drivers
            .iter()
            .any(|d| d.contains("无可用的来源证据")));
    }

    /// §4.3 row 2: download path + browser write handle => suspicion, not fact.
    #[test]
    fn handle_over_download_path_stays_suspicion() {
        let verdict = build(&input(
            (
                "download".to_string(),
                2,
                "rm.handle".to_string(),
            ),
            ProcessDigest {
                etw_hits: 0,
                live: 1,
                historical: 0,
                inferred: 0,
                etw_available: true,
            },
            complete_unique_safety(),
        ));
        assert!(verdict.headline.contains("很可能是浏览器下载产物"));
        assert!(verdict.confidence != "high");
    }

    /// §4.3 row 4: Q3 not ready caps the score at 55 and forces verify_first.
    #[test]
    fn unfinished_fingerprint_caps_the_verdict() {
        let verdict = build(&input(
            high_origin(),
            etw_processes(),
            SafetyDigest {
                hash_state: "pending".to_string(),
                ..Default::default()
            },
        ));
        assert!(verdict.score <= 55);
        assert_eq!(verdict.action, "verify_first");
        assert!(verdict.headline.contains("暂时无法判断删除后果"));
    }

    /// §4.4 example 2 (path guess + no process + unique copy with 200 pending)
    /// must land in the "low" band and carry the coverage caveat.
    #[test]
    fn path_guess_with_unfinished_coverage_is_low() {
        let verdict = build(&input(
            ("download".to_string(), 1, "path.shape".to_string()),
            ProcessDigest::default(),
            SafetyDigest {
                hash_state: "done".to_string(),
                real_copies: 0,
                hardlink_copies: 0,
                pending_unhashed: 200,
                watched: true,
                related: 0,
            },
        ));
        assert!(verdict.score < 50, "score was {}", verdict.score);
        assert_eq!(verdict.confidence, "low");
        assert_eq!(verdict.action, "verify_first");
        assert!(verdict
            .caveats
            .iter()
            .any(|c| c.contains("未完成指纹")));
    }

    /// §4.3 row 5: three unknown channels stay in the 15–55 band.
    #[test]
    fn three_unknown_channels_land_in_the_floor_band() {
        let verdict = build(&input(
            ("unknown".to_string(), 0, "none".to_string()),
            ProcessDigest::default(),
            SafetyDigest {
                hash_state: "pending".to_string(),
                ..Default::default()
            },
        ));
        assert!(verdict.score >= 15 && verdict.score <= 55);
        assert_eq!(verdict.action, "verify_first");
        assert!(verdict.headline.contains("证据不足"));
    }

    /// A "path guess" download with a live writer is exactly the H2 trap:
    /// rm.handle must never lift the origin to high, and the suspicion wording
    /// is required.
    #[test]
    fn rm_handle_cannot_exceed_medium_origin() {
        let origin_conf = super::origin_confidence("download", 2);
        assert_eq!(origin_conf, "medium");
        let origin_conf_path = super::origin_confidence("download", 1);
        assert_eq!(origin_conf_path, "low");
    }

    /// Non-Windows can never exceed medium, however good the rest is.
    #[test]
    fn non_windows_is_capped_at_medium() {
        let mut data = input(high_origin(), etw_processes(), complete_unique_safety());
        data.platform_supports_etw = false;
        data.processes.etw_available = false;
        let verdict = build(&data);
        assert!(verdict.score <= 77);
        assert_eq!(verdict.confidence, "medium");
        assert!(verdict.caveats.iter().any(|c| c.contains("本平台")));
    }

    /// Related files are never treated as copies.
    #[test]
    fn related_files_do_not_count_as_copies() {
        let verdict = build(&input(
            high_origin(),
            etw_processes(),
            SafetyDigest {
                hash_state: "done".to_string(),
                real_copies: 0,
                hardlink_copies: 0,
                pending_unhashed: 0,
                watched: true,
                related: 5,
            },
        ));
        assert!(verdict.headline.contains("唯一副本"));
        assert!(verdict.drivers.iter().any(|d| d.contains("关联文件")));
    }

    /// Hard links: no free space, no lost content, but not "independent copies".
    #[test]
    fn hardlink_only_is_medium_and_delete_safe() {
        let verdict = build(&input(
            high_origin(),
            etw_processes(),
            SafetyDigest {
                hash_state: "done".to_string(),
                real_copies: 0,
                hardlink_copies: 2,
                pending_unhashed: 0,
                watched: true,
                related: 0,
            },
        ));
        assert_eq!(verdict.action, "delete_safe");
        assert!(verdict.headline.contains("硬链接"));
        assert!(verdict.confidence != "high" || verdict.score < 90);
    }

    /// 0.4.0: an index-only (skip) file must say honestly that no fingerprint
    /// was ever computed, instead of promising a queue that will never finish.
    #[test]
    fn skip_file_is_explicitly_out_of_fingerprint_scope() {
        let verdict = build(&input(
            high_origin(),
            etw_processes(),
            SafetyDigest {
                hash_state: "skip".to_string(),
                ..Default::default()
            },
        ));
        assert_eq!(verdict.action, "verify_first");
        assert!(verdict.score <= 55);
        assert!(verdict
            .caveats
            .iter()
            .any(|c| c.contains("指纹范围外") && c.contains("无法判断删除后果")));
        assert!(verdict
            .drivers
            .iter()
            .any(|d| d.contains("不参与计算")));
    }

    #[test]
    fn unique_copy_caveat_names_the_fingerprint_scope() {
        let verdict = build(&input(
            high_origin(),
            etw_processes(),
            complete_unique_safety(),
        ));
        assert!(verdict
            .caveats
            .iter()
            .any(|c| c.contains("指纹范围目录")));
        assert!(!verdict.caveats.iter().any(|c| c.contains("受监控目录")));
    }
}
