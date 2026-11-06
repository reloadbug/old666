//! P5 悬浮球：位置 / 大小 / 透明度 / 收起状态的持久化。
//!
//! 悬浮球本身是一个独立的 webview 窗口（Tauri 的 `WebviewWindowBuilder`），
//! 这里只管**状态**：用户把它拖到哪、缩多大、多透明、是否收起 —— 这些要
//! 跨会话记住，所以落到 `data/floatball.json`。
//!
//! 为什么不把窗口操作放这里：窗口 API 属于 Tauri 层，放在 `ipc.rs` 里
//! 更符合分层（core 不依赖 UI 框架）。

use serde::{Deserialize, Serialize};

use super::store::data_dir;

/// 悬浮球状态。
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FloatBallState {
    /// 是否显示。
    pub visible: bool,
    /// 距屏幕左边缘（物理像素）。
    pub x: f64,
    /// 距屏幕顶部（物理像素）。
    pub y: f64,
    /// 球直径（物理像素）。
    pub size: f64,
    /// 不透明度 0.3–1.0。
    pub opacity: f32,
    /// 收起状态：只留一个窄条，不遮挡内容。
    pub collapsed: bool,
}

impl Default for FloatBallState {
    fn default() -> Self {
        Self {
            visible: false,
            // 屏幕右下角
            x: 1600.0,
            y: 800.0,
            size: 72.0,
            opacity: 0.92,
            collapsed: false,
        }
    }
}

impl FloatBallState {
    /// 夹紧到合理范围。尺寸别太小（点不中），也别大到挡屏。
    pub fn sanitize(mut self) -> Self {
        self.size = self.size.clamp(48.0, 160.0);
        self.opacity = self.opacity.clamp(0.3, 1.0);
        if !self.x.is_finite() {
            self.x = 0.0;
        }
        if !self.y.is_finite() {
            self.y = 0.0;
        }
        self
    }

    fn file() -> std::path::PathBuf {
        data_dir().join("floatball.json")
    }

    /// 读取状态。文件缺失或损坏时返回默认值 —— 悬浮球只是锦上添花，
    /// 不该因为一个坏 JSON 就让程序起不来。
    pub fn load() -> Self {
        std::fs::read_to_string(Self::file())
            .ok()
            .and_then(|s| serde_json::from_str::<Self>(&s).ok())
            .map(|s| s.sanitize())
            .unwrap_or_default()
    }

    /// 保存状态。写盘失败不影响内存里的状态。
    pub fn save(&self) {
        let s = self.clone().sanitize();
        if let Ok(text) = serde_json::to_string_pretty(&s) {
            let _ = std::fs::write(Self::file(), text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_hidden_and_sane() {
        let s = FloatBallState::default();
        assert!(!s.visible, "悬浮球默认不显示 —— 常驻监控不该一上来就抢注意力");
        assert!(s.size >= 48.0 && s.size <= 160.0);
        assert!(s.opacity >= 0.3 && s.opacity <= 1.0);
    }

    #[test]
    fn sanitize_clamps_size_and_opacity() {
        let s = FloatBallState {
            size: 9999.0,
            opacity: 5.0,
            ..Default::default()
        }
        .sanitize();
        assert_eq!(s.size, 160.0);
        assert_eq!(s.opacity, 1.0);

        let s = FloatBallState {
            size: 1.0,
            opacity: 0.0,
            ..Default::default()
        }
        .sanitize();
        assert_eq!(s.size, 48.0);
        assert_eq!(s.opacity, 0.3);
    }

    #[test]
    fn sanitize_fixes_non_finite_position() {
        let s = FloatBallState {
            x: f64::NAN,
            y: f64::INFINITY,
            ..Default::default()
        }
        .sanitize();
        assert!(s.x.is_finite());
        assert!(s.y.is_finite());
    }
}
