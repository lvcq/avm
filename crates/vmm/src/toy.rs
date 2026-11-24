//! toy guest：最小 16 位实模式引导镜像（M0 出口：串口打印）。
//!
//! 布局：镜像加载到 GPA 0x7c00，vcpu 从 CS=0 / RIP=0x7c00 起执行。
//! 代码通过 OUT 指令写串口 0x3F8（COM1 数据寄存器），无 BIOS 依赖。
//!
//! 手工汇编（NASM 语义，org 0x7c00）：
//! ```asm
//!   mov ah, 0            ; b4 00
//!   mov dx, 0x3f8        ; ba f8 03
//!   mov si, msg          ; be 14 7c
//! loop:                  ; <- 7c08
//!   lodsb                ; ac
//!   test al, al          ; 84 c0
//!   jz halt              ; 74 04
//!   out dx, al           ; ee
//!   jmp loop             ; eb f8
//! halt:                  ; <- 7c11
//!   hlt                  ; f4
//!   jmp halt             ; eb fe
//! msg: "avm toy guest OK\0"  ; <- 7c14
//! ```

/// toy guest 加载地址（GPA）。
pub const TOY_LOAD_GPA: u64 = 0x7c00;
/// 串口数据端口。
pub const SERIAL_PORT: u16 = 0x3f8;
/// 初始 RIP。
pub const TOY_ENTRY_RIP: u64 = 0x7c00;

/// 消息在镜像内的偏移（0x7c14 - 0x7c00）。
pub const MSG_OFFSET: usize = 0x14;

/// 生成 toy guest 引导镜像（512 字节，带 0x55AA 引导签名）。
pub fn toy_boot_sector() -> Vec<u8> {
    let mut img = vec![0u8; 512];

    let code: [u8; 19] = [
        0xb4, 0x00, // mov ah,0
        0xba, 0xf8, 0x03, // mov dx,0x3f8
        0xbe, 0x14, 0x7c, // mov si,0x7c14 (msg)
        0xac, // lodsb       <- 7c08
        0x84, 0xc0, // test al,al
        0x74, 0x03, // jz +3 -> halt(7c10)
        0xee, // out dx,al
        0xeb, 0xf8, // jmp loop(7c08)
        0xf4, // hlt         <- 7c10
        0xeb, 0xfd, // jmp halt(7c10)
    ];
    img[..code.len()].copy_from_slice(&code);

    const MSG_GPA: usize = MSG_OFFSET;
    let msg = b"avm toy guest OK\0";
    img[MSG_GPA..MSG_GPA + msg.len()].copy_from_slice(msg);

    // 引导签名
    img[510] = 0x55;
    img[511] = 0xAA;
    img
}

/// 从镜像中解出消息字符串（测试与调试用）。
pub fn toy_message() -> &'static str {
    "avm toy guest OK"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boot_sector_size_and_signature() {
        let img = toy_boot_sector();
        assert_eq!(img.len(), 512);
        assert_eq!(&img[510..], &[0x55, 0xAA]);
    }

    #[test]
    fn msg_offset_matches_code() {
        // 代码中 mov si 立即数必须等于消息实际偏移
        let img = toy_boot_sector();
        let si_imm = u16::from_le_bytes([img[6], img[7]]);
        assert_eq!(si_imm as usize, MSG_OFFSET + TOY_LOAD_GPA as usize);
        // 消息以 NUL 结尾，内容正确
        let bytes = &img[MSG_OFFSET..];
        let end = bytes.iter().position(|&b| b == 0).unwrap();
        assert_eq!(&bytes[..end], b"avm toy guest OK");
    }

    #[test]
    fn jump_offsets_resolve_to_lodsb_and_halt() {
        let img = toy_boot_sector();
        // lodsb 在 0x08；jmp loop 在 0x0e，位移 0xf8 -> 目标 0x08 ✓
        let jmp_loop_at = 0x0e;
        let disp = i8::from_le_bytes([img[jmp_loop_at + 1]]);
        assert_eq!((jmp_loop_at as i32 + 2 + disp as i32) as usize, 0x08);
        // jmp halt 在 0x11，位移 0xfd -> 目标 0x10（hlt）✓
        let jmp_halt_at = 0x11;
        let disp = i8::from_le_bytes([img[jmp_halt_at + 1]]);
        assert_eq!((jmp_halt_at as i32 + 2 + disp as i32) as usize, 0x10);
        assert_eq!(img[0x10], 0xf4); // hlt
    }

    #[test]
    fn message_is_printed_via_serial() {
        assert_eq!(toy_message(), "avm toy guest OK");
        assert_eq!(SERIAL_PORT, 0x3f8);
    }
}
