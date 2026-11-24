//! ACPI 表生成（M2）。
//!
//! 生成最小 RSDP + XSDT + SSDT 占位表。MADT/HPET 等在 M5 Android 启动前按需补充；
//! 表内容以 acpi_tables crate 构建并保证校验和正确。

use acpi_tables::madt::{
    EnabledStatus, IoApic, LocalInterruptController, ProcessorLocalApic, MADT,
};
use acpi_tables::{rsdp::Rsdp, sdt::Sdt, xsdt::XSDT, Aml};
use avm_core::{AvmError, AvmResult};

/// OEM ID（6 字节）。
const OEM_ID: [u8; 6] = *b"AVMRTM";

/// SSDT 在 guest 内存中的放置地址。
pub const SSDT_ADDR: u64 = 0x9_C000;
/// XSDT 放置地址。
pub const XSDT_ADDR: u64 = 0x9_B000;
/// MADT 放置地址。
pub const MADT_ADDR: u64 = 0x9_A000;
/// LAPIC 基地址（x86 标准 MMIO）。
pub const LAPIC_MMIO_ADDR: u32 = 0xFEE0_0000;
/// IO APIC 基地址。
pub const IOAPIC_MMIO_ADDR: u32 = 0xFEC0_0000;
/// RSDP 放置地址（BIOS 区域尾段）。
pub const RSDP_ADDR: u64 = 0x9_F000;

/// 生成的 ACPI 表集合（原始字节，由调用方写入 guest 内存）。
#[derive(Debug)]
pub struct AcpiTables {
    /// RSDP 原始字节（放置于 `RSDP_ADDR`）。
    pub rsdp: Vec<u8>,
    /// XSDT 原始字节（放置于 `XSDT_ADDR`）。
    pub xsdt: Vec<u8>,
    /// MADT 原始字节（放置于 `MADT_ADDR`）。
    pub madt: Vec<u8>,
    /// 各 SSDT 的原始字节与放置地址。
    pub ssdts: Vec<(u64, Vec<u8>)>,
}

/// 构建最小 ACPI 表集：XSDT 含 MADT（`vcpus` 个 LAPIC）+ SSDT 占位；RSDP 指向 XSDT。
pub fn build(vcpus: u32) -> AvmResult<AcpiTables> {
    if vcpus == 0 || vcpus > 255 {
        return Err(AvmError::Config(format!("vcpus={} 超出 [1,255]", vcpus)));
    }
    // SSDT：占位（64 字节，后续 M5 挂 PCI/设备描述）
    let mut ssdt = Sdt::new(*b"SSDT", 64, 2, OEM_ID, *b"AVMSSDT ", 1);
    ssdt.update_checksum();

    // MADT：每个 vcpu 一个 LAPIC + IO APIC
    let mut madt = MADT::new(
        OEM_ID,
        *b"AVMMADT ",
        1,
        LocalInterruptController::Address(LAPIC_MMIO_ADDR),
    );
    for i in 0..vcpus {
        madt.add_structure(ProcessorLocalApic::new(
            i as u8,
            i as u8,
            EnabledStatus::Enabled,
        ));
    }
    madt.add_structure(IoApic::new(0, IOAPIC_MMIO_ADDR, 0));
    let mut madt_bytes = Vec::new();
    madt.to_aml_bytes(&mut madt_bytes);

    // XSDT：登记 MADT + SSDT
    let mut xsdt = XSDT::new(OEM_ID, *b"AVMXSDT ", 1);
    xsdt.add_entry(MADT_ADDR);
    xsdt.add_entry(SSDT_ADDR);
    let mut xsdt_bytes = Vec::new();
    xsdt.to_aml_bytes(&mut xsdt_bytes);

    // RSDP：指向 XSDT（构造时即完成 checksum 计算）
    let rsdp = Rsdp::new(OEM_ID, XSDT_ADDR);
    let mut rsdp_bytes = Vec::new();
    rsdp.to_aml_bytes(&mut rsdp_bytes);

    Ok(AcpiTables {
        rsdp: rsdp_bytes,
        xsdt: xsdt_bytes,
        madt: madt_bytes,
        ssdts: vec![(SSDT_ADDR, ssdt.as_slice().to_vec())],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rsdp_signature_and_checksum() {
        let tables = build(2).unwrap();
        assert_eq!(&tables.rsdp[..8], b"RSD PTR ");
        let sum: u8 = tables.rsdp[..20]
            .iter()
            .fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(sum, 0, "RSDP 前 20 字节校验和应为 0");
        let esum: u8 = tables.rsdp.iter().fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(esum, 0, "RSDP 扩展校验和应为 0");
        let xsdt_addr = u64::from_le_bytes(tables.rsdp[24..32].try_into().unwrap());
        assert_eq!(xsdt_addr, XSDT_ADDR);
    }

    #[test]
    fn xsdt_points_to_ssdt_with_valid_checksum() {
        let tables = build(2).unwrap();
        assert_eq!(&tables.xsdt[..4], b"XSDT");
        let madt_addr = u64::from_le_bytes(tables.xsdt[36..44].try_into().unwrap());
        let ssdt_addr = u64::from_le_bytes(tables.xsdt[44..52].try_into().unwrap());
        assert_eq!(madt_addr, MADT_ADDR);
        assert_eq!(ssdt_addr, SSDT_ADDR);
        let sum: u8 = tables
            .xsdt
            .iter()
            .take(tables.xsdt[4] as usize)
            .fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(sum, 0, "XSDT 校验和应为 0");
    }

    #[test]
    fn madt_lapic_entries_match_vcpus() {
        let tables = build(4).unwrap();
        assert_eq!(&tables.madt[..4], b"APIC");
        // 表长 = 44(头) + 4×8(LAPIC) + 12(IOAPIC) = 88
        let len = u32::from_le_bytes(tables.madt[4..8].try_into().unwrap());
        assert_eq!(len, 44 + 4 * 8 + 12);
        let sum: u8 = tables
            .madt
            .iter()
            .take(len as usize)
            .fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(sum, 0, "MADT 校验和应为 0");
        // XSDT 登记了 MADT 与 SSDT 两个条目
        let e0 = u64::from_le_bytes(tables.xsdt[36..44].try_into().unwrap());
        let e1 = u64::from_le_bytes(tables.xsdt[44..52].try_into().unwrap());
        assert_eq!((e0, e1), (MADT_ADDR, SSDT_ADDR));
    }

    #[test]
    fn invalid_vcpus_rejected() {
        assert!(build(0).is_err());
        assert!(build(256).is_err());
    }

    #[test]
    fn ssdt_signature_and_checksum() {
        let tables = build(2).unwrap();
        let (_, ssdt) = &tables.ssdts[0];
        assert_eq!(&ssdt[..4], b"SSDT");
        let sum: u8 = ssdt
            .iter()
            .take(ssdt[4] as usize)
            .fold(0u8, |a, b| a.wrapping_add(*b));
        assert_eq!(sum, 0, "SSDT 校验和应为 0");
    }
}
