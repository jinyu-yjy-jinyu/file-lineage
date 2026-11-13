//! Instant-messaging receive-folder layout parsing.
//!
//! The IM clients that matter on a Chinese Windows desktop all use a very
//! regular directory shape, and it carries real information that a plain
//! "this came from WeChat" verdict throws away:
//!
//! ```text
//! Documents\xwechat_files\<account>_<suffix>\msg\file\2026-03\report.pdf
//! Documents\WeChat Files\<account>\FileStorage\File\2026-03\report.pdf
//! Documents\Tencent Files\<uin>\FileRecv\report.pdf
//! Documents\WXWork\<corp>\Cache\File\2026-03\report.pdf
//! ```
//!
//! From that we can answer *which account* and *which month* a file arrived
//! from, without reading a single byte of the client's private database.

#[derive(Debug, Clone, Default)]
pub struct ImLayout {
    pub vendor: String,
    /// Account / UIN / corp id directory, when the layout exposes one.
    pub account: Option<String>,
    /// `YYYY-MM` receive bucket, when present.
    pub month: Option<String>,
}

impl ImLayout {
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if let Some(account) = &self.account {
            parts.push(format!("账号 {}", account));
        }
        if let Some(month) = &self.month {
            parts.push(format!("接收于 {}", month));
        }
        if parts.is_empty() {
            format!("{} 接收目录结构", self.vendor)
        } else {
            format!("{}（{}）", self.vendor, parts.join("，"))
        }
    }
}

struct Vendor {
    marker: &'static str,
    name: &'static str,
}

/// Ordered longest-marker-first so `wechat files` wins over `wechat`.
const VENDORS: &[Vendor] = &[
    Vendor {
        marker: "xwechat_files",
        name: "微信 4.x",
    },
    Vendor {
        marker: "wechat files",
        name: "微信",
    },
    Vendor {
        marker: "wxwork",
        name: "企业微信",
    },
    Vendor {
        marker: "tencent files",
        name: "QQ",
    },
    Vendor {
        marker: "dingtalk",
        name: "钉钉",
    },
    Vendor {
        marker: "feishu",
        name: "飞书",
    },
    Vendor {
        marker: "lark",
        name: "飞书",
    },
    Vendor {
        marker: "telegram desktop",
        name: "Telegram",
    },
    Vendor {
        marker: "slack",
        name: "Slack",
    },
    Vendor {
        marker: "wechat_files",
        name: "微信",
    },
];

fn looks_like_month(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if bytes.len() != 7 || bytes[4] != b'-' {
        return false;
    }
    bytes[..4].iter().all(|b| b.is_ascii_digit()) && bytes[5..].iter().all(|b| b.is_ascii_digit())
}

/// `None` when the path is not inside a known IM receive tree.
pub fn parse(path: &str) -> Option<ImLayout> {
    let normalized = path.replace('/', "\\");
    let segments: Vec<&str> = normalized.split('\\').filter(|s| !s.is_empty()).collect();
    let lowered: Vec<String> = segments.iter().map(|s| s.to_lowercase()).collect();

    for vendor in VENDORS {
        let Some(index) = lowered.iter().position(|s| s == vendor.marker) else {
            continue;
        };
        let mut layout = ImLayout {
            vendor: vendor.name.to_string(),
            ..Default::default()
        };
        // The directory directly under the marker is the account / corp / UIN.
        if let Some(account) = segments.get(index + 1) {
            let trimmed = account.trim();
            if !trimmed.is_empty() && !looks_like_month(trimmed) {
                layout.account = Some(trimmed.to_string());
            }
        }
        // Any `YYYY-MM` bucket after the marker is a receive month.
        if let Some(month) = segments
            .iter()
            .skip(index + 1)
            .find(|s| looks_like_month(s))
        {
            layout.month = Some((*month).to_string());
        }
        return Some(layout);
    }
    None
}

/// Weak mail hint used only when nothing better is available.
pub fn mail_extension_hint(name: &str) -> Option<&'static str> {
    let lower = name.to_lowercase();
    if lower.ends_with(".msg") {
        Some("Outlook 邮件文件 (.msg)")
    } else if lower.ends_with(".eml") {
        Some("邮件导出文件 (.eml)")
    } else if lower.ends_with(".pst") || lower.ends_with(".ost") {
        Some("Outlook 数据文件")
    } else {
        None
    }
}
