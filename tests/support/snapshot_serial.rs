//! Deterministic arm64 guest used to certify serial snapshot continuation.

use bangbang_runtime::fdt::{ARM64_FDT_VMCLOCK_SIZE, ARM64_FDT_VMGENID_SIZE};
use bangbang_runtime::memory::aarch64;
use bangbang_runtime::serial::{
    SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE, SERIAL_INTERRUPT_IDENTIFICATION_FIFO_ENABLED,
    SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE, SERIAL_RECEIVE_FIFO_CAPACITY,
};

pub const SOURCE_READY_MARKER: &str = "BB_SERIAL_SNAPSHOT_SOURCE_READY\n";
pub const RESTORED_SUCCESS_MARKER: &str = "BB_SERIAL_SNAPSHOT_RESTORED_OK\n";
pub const RESTORED_FAILURE_MARKER: &str = "BB_SERIAL_SNAPSHOT_RESTORED_FAIL\n";
pub const CONFIGURED_SOURCE_MARKER: &str = "BB_SERIAL_CONFIGURED_SOURCE\n";
pub const CONFIGURED_RESTORED_MARKER: &str = "BB_SERIAL_CONFIGURED_RESTORED\n";
pub const CONFIGURED_FAILURE_MARKER: &str = "BB_SERIAL_CONFIGURED_FAIL\n";

pub const SOURCE_PREFIX_BYTE: u8 = b'A';
pub const SOURCE_ONLY_SUFFIX_BYTE: u8 = b'B';
pub const DESTINATION_SUFFIX_BYTE: u8 = b'C';
pub const SOURCE_PREFIX_LEN: usize = SERIAL_RECEIVE_FIFO_CAPACITY;
pub const DESTINATION_SUFFIX_LEN: usize = 40;

pub const INTERRUPT_ENABLE: u8 = SERIAL_INTERRUPT_ENABLE_RECEIVED_DATA_AVAILABLE;
pub const LINE_CONTROL: u8 = 0x1b;
pub const MODEM_CONTROL: u8 = 0x0b;
pub const SCRATCH: u8 = 0x5a;
pub const DIVISOR_LATCH_LOW: u8 = 0x34;
pub const DIVISOR_LATCH_HIGH: u8 = 0x12;
pub const INTERRUPT_IDENTIFICATION: u8 = SERIAL_INTERRUPT_IDENTIFICATION_FIFO_ENABLED
    | SERIAL_INTERRUPT_IDENTIFICATION_RECEIVED_DATA_AVAILABLE;

const IMAGE_HEADER_SIZE: usize = 64;
const IMAGE_MAGIC: u32 = 0x644d_5241;
const UART_ADDRESS: u64 = 0x4000_2000;
const VMGENID_ADDRESS: u64 = aarch64::SYSTEM_MEM_START + aarch64::SYSTEM_MEM_SIZE
    - ARM64_FDT_VMCLOCK_SIZE
    - ARM64_FDT_VMGENID_SIZE;

const UART_RECEIVE_TRANSMIT: u32 = 0;
const UART_INTERRUPT_ENABLE: u32 = 1;
const UART_INTERRUPT_IDENTIFICATION: u32 = 2;
const UART_LINE_CONTROL: u32 = 3;
const UART_MODEM_CONTROL: u32 = 4;
const UART_LINE_STATUS: u32 = 5;
const UART_SCRATCH: u32 = 7;
const UART_LINE_CONTROL_DLAB: u8 = 0x80;

const CONDITION_EQ: u32 = 0;
const CONDITION_NE: u32 = 1;

/// Complete source payload. Only the prefix can enter the bounded source UART;
/// the suffix deliberately remains in the source process pipe.
pub fn source_input() -> Vec<u8> {
    let mut input = vec![SOURCE_PREFIX_BYTE; SOURCE_PREFIX_LEN];
    input.extend(std::iter::repeat_n(
        SOURCE_ONLY_SUFFIX_BYTE,
        DESTINATION_SUFFIX_LEN,
    ));
    input
}

/// Bytes supplied only by a fresh destination process after snapshot load.
pub fn destination_input() -> Vec<u8> {
    vec![DESTINATION_SUFFIX_BYTE; DESTINATION_SUFFIX_LEN]
}

/// Builds the default-stdio guest that validates restored registers and RX.
pub fn default_stdio_guest_image() -> Vec<u8> {
    let mut assembler = Assembler::default();
    emit_load_address(&mut assembler, 8, VMGENID_ADDRESS);
    assembler.emit(ldp_x(20, 21, 8));
    emit_load_address(&mut assembler, 4, UART_ADDRESS);
    emit_configure_uart(&mut assembler);
    emit_serial_text(&mut assembler, SOURCE_READY_MARKER);

    let vmgenid_poll = assembler.position();
    assembler.emit(ldp_x(22, 23, 8));
    assembler.emit(cmp_x(22, 20));
    let first_changed = assembler.emit_branch_placeholder();
    assembler.emit(cmp_x(23, 21));
    assembler.emit(b_cond(vmgenid_poll, assembler.position(), CONDITION_EQ));
    let restored = assembler.position();
    assembler.patch_conditional(first_changed, restored, CONDITION_NE);

    let mut failures = Vec::new();
    emit_verify_register(
        &mut assembler,
        UART_LINE_CONTROL,
        LINE_CONTROL,
        &mut failures,
    );
    emit_verify_register(
        &mut assembler,
        UART_MODEM_CONTROL,
        MODEM_CONTROL,
        &mut failures,
    );
    emit_verify_register(&mut assembler, UART_SCRATCH, SCRATCH, &mut failures);
    emit_verify_register(
        &mut assembler,
        UART_INTERRUPT_ENABLE,
        INTERRUPT_ENABLE,
        &mut failures,
    );
    emit_verify_register(
        &mut assembler,
        UART_INTERRUPT_IDENTIFICATION,
        INTERRUPT_IDENTIFICATION,
        &mut failures,
    );

    assembler.emit(ldrb_w(13, 4, UART_LINE_STATUS));
    failures.push(FailureBranch::TestBitZero {
        index: assembler.emit_branch_placeholder(),
        register: 13,
        bit: 0,
    });

    assembler.emit(movz_x(7, u16::from(UART_LINE_CONTROL_DLAB), 0));
    assembler.emit(strb_w(7, 4, UART_LINE_CONTROL));
    emit_verify_register(
        &mut assembler,
        UART_RECEIVE_TRANSMIT,
        DIVISOR_LATCH_LOW,
        &mut failures,
    );
    emit_verify_register(
        &mut assembler,
        UART_INTERRUPT_ENABLE,
        DIVISOR_LATCH_HIGH,
        &mut failures,
    );
    assembler.emit(movz_x(7, u16::from(LINE_CONTROL), 0));
    assembler.emit(strb_w(7, 4, UART_LINE_CONTROL));

    emit_receive_run(
        &mut assembler,
        SOURCE_PREFIX_LEN,
        SOURCE_PREFIX_BYTE,
        &mut failures,
    );
    emit_receive_run(
        &mut assembler,
        DESTINATION_SUFFIX_LEN,
        DESTINATION_SUFFIX_BYTE,
        &mut failures,
    );
    emit_serial_text(&mut assembler, RESTORED_SUCCESS_MARKER);
    emit_system_off(&mut assembler);

    let failure = assembler.position();
    for branch in failures {
        assembler.patch_failure(branch, failure);
    }
    emit_serial_text(&mut assembler, RESTORED_FAILURE_MARKER);
    emit_system_off(&mut assembler);
    finish_image(assembler.instructions)
}

/// Builds the configured-output guest. It validates non-RX register continuity
/// and emits a destination marker before orderly shutdown.
pub fn configured_output_guest_image() -> Vec<u8> {
    let mut assembler = Assembler::default();
    emit_load_address(&mut assembler, 8, VMGENID_ADDRESS);
    assembler.emit(ldp_x(20, 21, 8));
    emit_load_address(&mut assembler, 4, UART_ADDRESS);
    emit_configure_uart(&mut assembler);
    emit_serial_text(&mut assembler, CONFIGURED_SOURCE_MARKER);

    let vmgenid_poll = assembler.position();
    assembler.emit(ldp_x(22, 23, 8));
    assembler.emit(cmp_x(22, 20));
    let first_changed = assembler.emit_branch_placeholder();
    assembler.emit(cmp_x(23, 21));
    assembler.emit(b_cond(vmgenid_poll, assembler.position(), CONDITION_EQ));
    let restored = assembler.position();
    assembler.patch_conditional(first_changed, restored, CONDITION_NE);

    let mut failures = Vec::new();
    emit_verify_register(
        &mut assembler,
        UART_LINE_CONTROL,
        LINE_CONTROL,
        &mut failures,
    );
    emit_verify_register(
        &mut assembler,
        UART_MODEM_CONTROL,
        MODEM_CONTROL,
        &mut failures,
    );
    emit_verify_register(&mut assembler, UART_SCRATCH, SCRATCH, &mut failures);
    emit_serial_text(&mut assembler, CONFIGURED_RESTORED_MARKER);
    emit_system_off(&mut assembler);

    let failure = assembler.position();
    for branch in failures {
        assembler.patch_failure(branch, failure);
    }
    emit_serial_text(&mut assembler, CONFIGURED_FAILURE_MARKER);
    emit_system_off(&mut assembler);
    finish_image(assembler.instructions)
}

/// Checks stable header and protocol facts without executing HVF.
pub fn assert_guest_images() {
    for image in [default_stdio_guest_image(), configured_output_guest_image()] {
        assert_eq!(read_u32(&image, 0), 0x1400_0010);
        assert_eq!(read_u32(&image, 4), 0xd503_201f);
        assert_eq!(read_u64(&image, 8), 0);
        assert_eq!(read_u32(&image, 56), IMAGE_MAGIC);
        assert_eq!(
            read_u64(&image, 16),
            u64::try_from(image.len()).expect("serial guest image length should fit")
        );
        assert!(image.len() > IMAGE_HEADER_SIZE);
        assert!(image.len() < 16 * 1024);
    }
    assert_eq!(
        source_input().len(),
        SOURCE_PREFIX_LEN + DESTINATION_SUFFIX_LEN
    );
    assert!(source_input().len() > SERIAL_RECEIVE_FIFO_CAPACITY);
    assert_eq!(destination_input().len(), DESTINATION_SUFFIX_LEN);
}

fn emit_configure_uart(assembler: &mut Assembler) {
    assembler.emit(movz_x(7, u16::from(UART_LINE_CONTROL_DLAB), 0));
    assembler.emit(strb_w(7, 4, UART_LINE_CONTROL));
    assembler.emit(movz_x(7, u16::from(DIVISOR_LATCH_LOW), 0));
    assembler.emit(strb_w(7, 4, UART_RECEIVE_TRANSMIT));
    assembler.emit(movz_x(7, u16::from(DIVISOR_LATCH_HIGH), 0));
    assembler.emit(strb_w(7, 4, UART_INTERRUPT_ENABLE));
    assembler.emit(movz_x(7, u16::from(LINE_CONTROL), 0));
    assembler.emit(strb_w(7, 4, UART_LINE_CONTROL));
    assembler.emit(movz_x(7, u16::from(MODEM_CONTROL), 0));
    assembler.emit(strb_w(7, 4, UART_MODEM_CONTROL));
    assembler.emit(movz_x(7, u16::from(SCRATCH), 0));
    assembler.emit(strb_w(7, 4, UART_SCRATCH));
    assembler.emit(movz_x(7, u16::from(INTERRUPT_ENABLE), 0));
    assembler.emit(strb_w(7, 4, UART_INTERRUPT_ENABLE));
}

fn emit_verify_register(
    assembler: &mut Assembler,
    offset: u32,
    expected: u8,
    failures: &mut Vec<FailureBranch>,
) {
    assembler.emit(ldrb_w(9, 4, offset));
    assembler.emit(cmp_w_imm(9, u16::from(expected)));
    failures.push(FailureBranch::Conditional {
        index: assembler.emit_branch_placeholder(),
        condition: CONDITION_NE,
    });
}

fn emit_receive_run(
    assembler: &mut Assembler,
    count: usize,
    expected: u8,
    failures: &mut Vec<FailureBranch>,
) {
    assembler.emit(movz_x(
        12,
        u16::try_from(count).expect("serial receive run count should fit"),
        0,
    ));
    let poll = assembler.position();
    assembler.emit(ldrb_w(13, 4, UART_LINE_STATUS));
    assembler.emit(tbz(13, 0, poll, assembler.position()));
    assembler.emit(ldrb_w(14, 4, UART_RECEIVE_TRANSMIT));
    assembler.emit(cmp_w_imm(14, u16::from(expected)));
    failures.push(FailureBranch::Conditional {
        index: assembler.emit_branch_placeholder(),
        condition: CONDITION_NE,
    });
    assembler.emit(subs_w_imm(12, 12, 1));
    assembler.emit(b_cond(poll, assembler.position(), CONDITION_NE));
}

fn emit_serial_text(assembler: &mut Assembler, text: &str) {
    for byte in text.bytes() {
        assembler.emit(movz_x(7, u16::from(byte), 0));
        assembler.emit(strb_w(7, 4, UART_RECEIVE_TRANSMIT));
    }
}

fn emit_system_off(assembler: &mut Assembler) {
    assembler.emit(movz_x(0, 0x0008, 0));
    assembler.emit(movk_x(0, 0x8400, 16));
    assembler.emit(0xd400_0002); // hvc #0 (PSCI_SYSTEM_OFF)
    assembler.emit(0x1400_0000); // b . if the host unexpectedly returns
}

fn emit_load_address(assembler: &mut Assembler, register: u32, address: u64) {
    assert_eq!(address >> 32, 0);
    assembler.emit(movz_x(register, low_u16(address, 0), 0));
    assembler.emit(movk_x(register, low_u16(address, 16), 16));
}

fn finish_image(instructions: Vec<u32>) -> Vec<u8> {
    let mut image = vec![0; IMAGE_HEADER_SIZE];
    write_u32(&mut image, 0, 0x1400_0010);
    write_u32(&mut image, 4, 0xd503_201f);
    write_u64(&mut image, 8, 0);
    write_u32(&mut image, 56, IMAGE_MAGIC);
    image.extend(instructions.into_iter().flat_map(u32::to_le_bytes));
    let image_size = u64::try_from(image.len()).expect("serial guest image length should fit");
    write_u64(&mut image, 16, image_size);
    image
}

#[derive(Default)]
struct Assembler {
    instructions: Vec<u32>,
}

enum FailureBranch {
    Conditional {
        index: usize,
        condition: u32,
    },
    TestBitZero {
        index: usize,
        register: u32,
        bit: u32,
    },
}

impl Assembler {
    fn position(&self) -> usize {
        self.instructions.len()
    }

    fn emit(&mut self, instruction: u32) {
        self.instructions.push(instruction);
    }

    fn emit_branch_placeholder(&mut self) -> usize {
        let index = self.position();
        self.emit(0);
        index
    }

    fn patch_conditional(&mut self, index: usize, target: usize, condition: u32) {
        self.instructions[index] = b_cond(target, index, condition);
    }

    fn patch_failure(&mut self, branch: FailureBranch, target: usize) {
        match branch {
            FailureBranch::Conditional { index, condition } => {
                self.patch_conditional(index, target, condition);
            }
            FailureBranch::TestBitZero {
                index,
                register,
                bit,
            } => {
                self.instructions[index] = tbz(register, bit, target, index);
            }
        }
    }
}

fn movz_x(register: u32, immediate: u16, shift: u32) -> u32 {
    assert!(register <= 30);
    assert!(shift <= 48 && shift.is_multiple_of(16));
    0xd280_0000 | ((shift / 16) << 21) | (u32::from(immediate) << 5) | register
}

fn movk_x(register: u32, immediate: u16, shift: u32) -> u32 {
    assert!(register <= 30);
    assert!(shift <= 48 && shift.is_multiple_of(16));
    0xf280_0000 | ((shift / 16) << 21) | (u32::from(immediate) << 5) | register
}

fn ldp_x(first: u32, second: u32, base: u32) -> u32 {
    assert!(first <= 30 && second <= 30 && base <= 30);
    0xa940_0000 | (second << 10) | (base << 5) | first
}

fn cmp_x(left: u32, right: u32) -> u32 {
    assert!(left <= 30 && right <= 30);
    0xeb00_001f | (right << 16) | (left << 5)
}

fn ldrb_w(destination: u32, base: u32, byte_offset: u32) -> u32 {
    assert!(destination <= 30 && base <= 30 && byte_offset <= 0xfff);
    0x3940_0000 | (byte_offset << 10) | (base << 5) | destination
}

fn strb_w(source: u32, base: u32, byte_offset: u32) -> u32 {
    assert!(source <= 30 && base <= 30 && byte_offset <= 0xfff);
    0x3900_0000 | (byte_offset << 10) | (base << 5) | source
}

fn cmp_w_imm(register: u32, immediate: u16) -> u32 {
    assert!(register <= 30 && immediate <= 0x0fff);
    0x7100_001f | (u32::from(immediate) << 10) | (register << 5)
}

fn subs_w_imm(destination: u32, source: u32, immediate: u16) -> u32 {
    assert!(destination <= 30 && source <= 30 && immediate <= 0x0fff);
    0x7100_0000 | (u32::from(immediate) << 10) | (source << 5) | destination
}

fn b_cond(target: usize, source: usize, condition: u32) -> u32 {
    assert!(condition <= 0xf);
    let offset = branch_offset(target, source);
    assert!((-262_144..262_144).contains(&offset));
    0x5400_0000 | ((offset.cast_unsigned() & 0x7_ffff) << 5) | condition
}

fn tbz(register: u32, bit: u32, target: usize, source: usize) -> u32 {
    assert!(register <= 30 && bit < 32);
    let offset = branch_offset(target, source);
    assert!((-8192..8192).contains(&offset));
    0x3600_0000 | (bit << 19) | ((offset.cast_unsigned() & 0x3fff) << 5) | register
}

fn branch_offset(target: usize, source: usize) -> i32 {
    i32::try_from(target)
        .expect("branch target should fit")
        .checked_sub(i32::try_from(source).expect("branch source should fit"))
        .expect("branch offset should fit")
}

fn low_u16(value: u64, shift: u32) -> u16 {
    u16::try_from((value >> shift) & u64::from(u16::MAX))
        .expect("masked address immediate should fit")
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("u32 image field should fit"),
    )
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        bytes[offset..offset + 8]
            .try_into()
            .expect("u64 image field should fit"),
    )
}
