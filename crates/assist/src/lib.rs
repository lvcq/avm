//! avm-assist：adb 通道与辅助能力。
//!
//! 提供 adb 连接管理、截图、输入注入等供外部自动化工具
//! （Airtest / Appium / uiautomator2）消费的底层能力。

use avm_core::{AvmError, AvmResult};

/// 一条 adb 通道（TCP 连接或 USB 序列号）。
#[derive(Debug, Clone)]
pub struct AdbChannel {
    pub target: String,
}

impl AdbChannel {
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
        }
    }

    /// M3 后可用：连接并等待 device 状态。
    pub fn connect(&self) -> AvmResult<()> {
        Err(AvmError::Ipc(format!(
            "adb connect {} : M3 里程碑实现",
            self.target
        )))
    }

    /// 截图（依赖 M4a 2D scanout 后的帧缓冲）。
    pub fn screenshot(&self) -> AvmResult<Vec<u8>> {
        Err(AvmError::Device("screenshot: M4a 后实现".into()))
    }

    /// 输入注入（tap / swipe / key）。
    pub fn inject_tap(&self, _x: i32, _y: i32) -> AvmResult<()> {
        Err(AvmError::Device("inject: M5 后实现".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_builds() {
        let c = AdbChannel::new("127.0.0.1:5555");
        assert_eq!(c.target, "127.0.0.1:5555");
    }
}
