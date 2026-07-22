use std::cell::Cell;
use std::fmt;
use std::net::Ipv4Addr;
use std::num::{NonZeroU16, NonZeroU64};

use super::handler::MmdsTcpHandler;
use super::*;

const LOCAL_PORT: u16 = 80;
const REMOTE_PORT: u16 = 49_152;
const REMOTE_ADDRESS: Ipv4Addr = Ipv4Addr::new(192, 0, 2, 10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CallbackError;

impl fmt::Display for CallbackError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("callback error")
    }
}

impl std::error::Error for CallbackError {}

fn no_response(_: &[u8]) -> Result<Vec<u8>, CallbackError> {
    Ok(Vec::new())
}

fn segment_bytes(
    ports: (u16, u16),
    sequence_number: u32,
    acknowledgement_number: u32,
    flags: TcpFlags,
    window_size: u16,
    options: &[u8],
    payload: &[u8],
) -> Vec<u8> {
    let (source_port, destination_port) = ports;
    assert_eq!(options.len() % 4, 0);
    let header_len = 20 + options.len();
    assert!(header_len <= 60);
    let mut bytes = Vec::with_capacity(header_len + payload.len());
    bytes.extend_from_slice(&source_port.to_be_bytes());
    bytes.extend_from_slice(&destination_port.to_be_bytes());
    bytes.extend_from_slice(&sequence_number.to_be_bytes());
    bytes.extend_from_slice(&acknowledgement_number.to_be_bytes());
    bytes.push(u8::try_from(header_len / 4).expect("test header length fits") << 4);
    bytes.push(flags.bits());
    bytes.extend_from_slice(&window_size.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(options);
    bytes.extend_from_slice(payload);
    bytes
}

fn basic_segment(
    sequence_number: u32,
    acknowledgement_number: u32,
    flags: TcpFlags,
    window_size: u16,
    payload: &[u8],
) -> Vec<u8> {
    segment_bytes(
        (REMOTE_PORT, LOCAL_PORT),
        sequence_number,
        acknowledgement_number,
        flags,
        window_size,
        &[],
        payload,
    )
}

fn syn_segment(sequence_number: u32, window_size: u16, mss: Option<u16>) -> Vec<u8> {
    let options = mss.map_or_else(Vec::new, |value| {
        let mut option = vec![2, 4];
        option.extend_from_slice(&value.to_be_bytes());
        option
    });
    segment_bytes(
        (REMOTE_PORT, LOCAL_PORT),
        sequence_number,
        0,
        TcpFlags::SYNCHRONIZE,
        window_size,
        &options,
        &[],
    )
}

fn open_connection(
    remote_initial_sequence: u32,
    local_initial_sequence: u32,
    local_window: u32,
    remote_window: u16,
    mss: Option<u16>,
    retransmission: (u64, u16),
    now: u64,
) -> Connection {
    let (retransmission_period, retransmission_limit) = retransmission;
    let syn_bytes = syn_segment(remote_initial_sequence, remote_window, mss);
    let syn = TcpSegment::parse(&syn_bytes).expect("test SYN parses");
    Connection::passive_open(
        &syn,
        local_window,
        NonZeroU64::new(retransmission_period).expect("test RTO is nonzero"),
        NonZeroU16::new(retransmission_limit).expect("test retry limit is nonzero"),
        local_initial_sequence,
        now,
    )
    .expect("test passive open succeeds")
}

fn establish_connection(
    connection: &mut Connection,
    remote_initial_sequence: u32,
    local_initial_sequence: u32,
    remote_window: u16,
    now: u64,
) {
    let mut output = [];
    let syn_ack = connection
        .write_next_segment(&mut output, 0, None, now)
        .expect("test SYN-ACK write succeeds")
        .expect("test SYN-ACK is available");
    assert_eq!(syn_ack.flags(), TcpFlags::SYNCHRONIZE | TcpFlags::ACK);
    assert_eq!(syn_ack.sequence_number(), local_initial_sequence);
    assert_eq!(
        syn_ack.acknowledgement_number(),
        remote_initial_sequence.wrapping_add(1)
    );

    let ack_bytes = basic_segment(
        remote_initial_sequence.wrapping_add(1),
        local_initial_sequence.wrapping_add(1),
        TcpFlags::ACK,
        remote_window,
        &[],
    );
    let ack = TcpSegment::parse(&ack_bytes).expect("test ACK parses");
    let mut receive = [];
    assert_eq!(
        connection
            .receive_segment(&ack, &mut receive, now)
            .expect("test ACK is accepted"),
        (0, ReceiveStatus::empty())
    );
    assert!(connection.is_established());
}

fn establish_endpoint(
    remote_initial_sequence: u32,
    local_initial_sequence: u32,
    mss: Option<u16>,
    now: u64,
) -> Endpoint {
    let syn_bytes = syn_segment(remote_initial_sequence, 4_096, mss);
    let syn = TcpSegment::parse(&syn_bytes).expect("test SYN parses");
    let mut endpoint =
        Endpoint::new(&syn, local_initial_sequence, now).expect("test endpoint opens");
    let mut output = [];
    let syn_ack = endpoint
        .write_next_segment(&mut output, 0, now)
        .expect("test SYN-ACK write succeeds")
        .expect("test SYN-ACK is available");
    assert_eq!(syn_ack.sequence_number(), local_initial_sequence);
    let ack_bytes = basic_segment(
        remote_initial_sequence.wrapping_add(1),
        local_initial_sequence.wrapping_add(1),
        TcpFlags::ACK,
        4_096,
        &[],
    );
    let ack = TcpSegment::parse(&ack_bytes).expect("test ACK parses");
    assert_eq!(
        endpoint
            .receive_segment(&ack, now, no_response)
            .expect("test handshake ACK succeeds"),
        ReceiveStatus::empty()
    );
    endpoint
}

#[test]
fn segment_parser_is_bounded_and_strict() {
    assert_eq!(
        TcpSegment::parse(&[0; 19]).expect_err("short segment is rejected"),
        SegmentParseError::SliceTooShort { len: 19 }
    );

    let mut invalid_header = basic_segment(1, 2, TcpFlags::ACK, 10, &[]);
    invalid_header[12] = 4 << 4;
    assert!(matches!(
        TcpSegment::parse(&invalid_header),
        Err(SegmentParseError::HeaderLength {
            header_len: 16,
            segment_len: 20
        })
    ));
    invalid_header[12] = 15 << 4;
    assert!(matches!(
        TcpSegment::parse(&invalid_header),
        Err(SegmentParseError::HeaderLength {
            header_len: 60,
            segment_len: 20
        })
    ));

    let malformed_option = segment_bytes(
        (REMOTE_PORT, LOCAL_PORT),
        1,
        0,
        TcpFlags::SYNCHRONIZE,
        10,
        &[0xff, 0, 0, 0],
        &[],
    );
    assert!(matches!(
        TcpSegment::parse(&malformed_option),
        Err(SegmentParseError::InvalidOptionLength { offset: 0, len: 0 })
    ));

    let truncated_option = segment_bytes(
        (REMOTE_PORT, LOCAL_PORT),
        1,
        0,
        TcpFlags::SYNCHRONIZE,
        10,
        &[0xff, 8, 0, 0],
        &[],
    );
    assert!(matches!(
        TcpSegment::parse(&truncated_option),
        Err(SegmentParseError::TruncatedOptionValue { .. })
    ));

    let low_mss = syn_segment(1, 10, Some(MIN_MSS - 1));
    assert_eq!(
        TcpSegment::parse(&low_mss).expect_err("small MSS is rejected"),
        SegmentParseError::InvalidMssValue { value: MIN_MSS - 1 }
    );

    let duplicate_mss = segment_bytes(
        (REMOTE_PORT, LOCAL_PORT),
        1,
        0,
        TcpFlags::SYNCHRONIZE,
        10,
        &[2, 4, 0, 100, 2, 4, 0, 101],
        &[],
    );
    assert_eq!(
        TcpSegment::parse(&duplicate_mss).expect_err("duplicate MSS is rejected"),
        SegmentParseError::DuplicateMss
    );

    let payload = b"secret payload";
    let valid = segment_bytes(
        (REMOTE_PORT, LOCAL_PORT),
        123,
        456,
        TcpFlags::ACK | TcpFlags::PUSH,
        789,
        &[1, 1, 0, 0],
        payload,
    );
    let parsed = TcpSegment::parse(&valid).expect("valid segment parses");
    assert_eq!(parsed.source_port(), REMOTE_PORT);
    assert_eq!(parsed.destination_port(), LOCAL_PORT);
    assert_eq!(parsed.sequence_number(), 123);
    assert_eq!(parsed.acknowledgement_number(), 456);
    assert_eq!(parsed.window_size(), 789);
    assert_eq!(parsed.payload(), payload);
    assert!(!format!("{parsed:?}").contains("secret payload"));
}

#[test]
fn malformed_segment_matrix_never_panics_or_owns_input() {
    for len in 0..=80 {
        for data_offset_words in 0_u8..=15 {
            let mut bytes = vec![0xff; len];
            if let Some(data_offset) = bytes.get_mut(12) {
                *data_offset = data_offset_words << 4;
            }
            let _ = TcpSegment::parse(&bytes);
        }
    }

    let oversized = vec![0; usize::from(u16::MAX) + 1];
    assert_eq!(
        TcpSegment::parse(&oversized).expect_err("oversized segment is rejected"),
        SegmentParseError::SegmentTooLong {
            len: usize::from(u16::MAX) + 1
        }
    );
}

#[test]
fn sequence_and_reset_rules_match_the_pinned_core() {
    let reference = u32::MAX - 3;
    let wrapped = 2;
    assert!(sequence_after(wrapped, reference));
    assert!(sequence_at_or_after(wrapped, reference));
    assert!(sequence_at_or_after(reference, reference));
    assert!(!sequence_after(reference, reference));
    assert!(!sequence_after(
        reference.wrapping_add(MAX_WINDOW_SIZE),
        reference
    ));

    let without_ack = basic_segment(100, 900, TcpFlags::PUSH, 10, b"abc");
    let without_ack = TcpSegment::parse(&without_ack).expect("test segment parses");
    assert_eq!(
        ResetConfig::from_segment(&without_ack),
        ResetConfig::Acknowledgement(103)
    );

    let with_ack = basic_segment(100, 900, TcpFlags::ACK, 10, b"abc");
    let with_ack = TcpSegment::parse(&with_ack).expect("test segment parses");
    assert_eq!(
        ResetConfig::from_segment(&with_ack),
        ResetConfig::Sequence(900)
    );
}

#[test]
fn connection_handshake_retransmits_and_uses_default_or_explicit_mss() {
    let remote_initial = 10_000;
    let local_initial = 20_000;
    let period = 100;
    let mut connection = open_connection(
        remote_initial,
        local_initial,
        2_500,
        4_096,
        None,
        (period, 15),
        0,
    );
    assert_eq!(connection.maximum_segment_size(), DEFAULT_MSS);
    assert_eq!(
        connection.control_segment_or_timeout_status(),
        NextSegmentStatus::Available
    );

    let mut output = [];
    let syn_ack = connection
        .write_next_segment(&mut output, 0, None, 0)
        .expect("SYN-ACK write succeeds")
        .expect("SYN-ACK is available");
    assert_eq!(syn_ack.maximum_segment_size(), Some(DEFAULT_MSS));
    assert_eq!(
        connection.control_segment_or_timeout_status(),
        NextSegmentStatus::Timeout(period)
    );
    assert!(
        connection
            .write_next_segment(&mut output, 0, None, period - 1)
            .expect("early output check succeeds")
            .is_none()
    );
    let retransmission = connection
        .write_next_segment(&mut output, 0, None, period)
        .expect("timeout retransmission succeeds")
        .expect("SYN-ACK retransmission is available");
    assert_eq!(
        retransmission.flags(),
        TcpFlags::SYNCHRONIZE | TcpFlags::ACK
    );

    let syn_bytes = syn_segment(remote_initial, 4_096, None);
    let syn = TcpSegment::parse(&syn_bytes).expect("test SYN parses");
    let mut receive = [];
    assert_eq!(
        connection
            .receive_segment(&syn, &mut receive, period)
            .expect("repeated SYN succeeds"),
        (0, ReceiveStatus::empty())
    );
    assert_eq!(
        connection.control_segment_or_timeout_status(),
        NextSegmentStatus::Available
    );

    let explicit = open_connection(1, 2, 100, 100, Some(1_200), (10, 2), 0);
    assert_eq!(explicit.maximum_segment_size(), 1_200);

    let invalid = basic_segment(1, 0, TcpFlags::ACK, 100, &[]);
    let invalid = TcpSegment::parse(&invalid).expect("test invalid SYN input parses");
    assert!(matches!(
        Connection::passive_open(
            &invalid,
            100,
            NonZeroU64::new(1).expect("nonzero"),
            NonZeroU16::new(1).expect("nonzero"),
            2,
            0,
        ),
        Err(PassiveOpenError::InvalidSyn)
    ));
}

#[test]
fn connection_receive_window_reorder_duplicate_and_wrap_are_bounded() {
    let remote_initial = u32::MAX - 2;
    let local_initial = 1_000;
    let mut connection = open_connection(
        remote_initial,
        local_initial,
        4,
        4_096,
        Some(400),
        (100, 15),
        0,
    );
    establish_connection(&mut connection, remote_initial, local_initial, 4_096, 0);

    let reordered = basic_segment(
        remote_initial.wrapping_add(2),
        local_initial.wrapping_add(1),
        TcpFlags::ACK,
        4_096,
        b"x",
    );
    let reordered = TcpSegment::parse(&reordered).expect("reordered segment parses");
    let mut receive = [0; 4];
    let (_, status) = connection
        .receive_segment(&reordered, &mut receive, 1)
        .expect("reordered segment is classified");
    assert!(status.intersects(ReceiveStatus::UNEXPECTED_SEQUENCE));

    let ordered = basic_segment(
        remote_initial.wrapping_add(1),
        local_initial.wrapping_add(1),
        TcpFlags::ACK,
        4_096,
        b"abcd",
    );
    let ordered = TcpSegment::parse(&ordered).expect("ordered segment parses");
    assert_eq!(
        connection
            .receive_segment(&ordered, &mut receive, 2)
            .expect("ordered segment is accepted"),
        (4, ReceiveStatus::empty())
    );
    assert_eq!(&receive, b"abcd");

    let beyond = basic_segment(
        remote_initial.wrapping_add(5),
        local_initial.wrapping_add(1),
        TcpFlags::ACK,
        4_096,
        b"e",
    );
    let beyond = TcpSegment::parse(&beyond).expect("beyond-window segment parses");
    let (_, status) = connection
        .receive_segment(&beyond, &mut receive, 3)
        .expect("beyond-window segment is classified");
    assert!(status.intersects(ReceiveStatus::SEGMENT_BEYOND_RECEIVE_WINDOW));

    connection.advance_local_receive_window(1);
    assert_eq!(
        connection
            .receive_segment(&beyond, &mut receive, 4)
            .expect("reopened window accepts byte"),
        (1, ReceiveStatus::empty())
    );
    assert_eq!(
        connection.highest_acknowledgement_received(),
        local_initial.wrapping_add(1)
    );

    let duplicate = ordered;
    let (_, status) = connection
        .receive_segment(&duplicate, &mut receive, 5)
        .expect("duplicate data is classified");
    assert!(status.intersects(ReceiveStatus::UNEXPECTED_SEQUENCE));

    let mut output = [];
    let acknowledgement = connection
        .write_next_segment(&mut output, 0, None, 5)
        .expect("ACK write succeeds")
        .expect("ACK is pending");
    assert_eq!(acknowledgement.flags(), TcpFlags::ACK);
    assert_eq!(acknowledgement.acknowledgement_number(), 3);
}

#[test]
fn connection_segments_replays_partial_ack_and_retransmits() {
    let remote_initial = 100;
    let local_initial = 1_000;
    let period = 50;
    let mut connection = open_connection(
        remote_initial,
        local_initial,
        2_500,
        2_000,
        Some(300),
        (period, 15),
        0,
    );
    establish_connection(&mut connection, remote_initial, local_initial, 2_000, 0);

    let mut payload = vec![0; 700];
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte = u8::try_from(index % 251).expect("test byte fits");
    }
    let initial_sequence = local_initial.wrapping_add(1);
    let source = PayloadSource::new(&payload, initial_sequence);
    let mut output = [0; 512];
    let first = connection
        .write_next_segment(&mut output, 50, Some(source), 0)
        .expect("first data segment succeeds")
        .expect("first data segment exists");
    assert_eq!(first.payload_len(), 250);
    assert_eq!(&output[..250], &payload[..250]);
    let second = connection
        .write_next_segment(&mut output, 50, Some(source), 0)
        .expect("second data segment succeeds")
        .expect("second data segment exists");
    assert_eq!(second.sequence_number(), initial_sequence.wrapping_add(250));
    assert_eq!(second.payload_len(), 250);
    let third = connection
        .write_next_segment(&mut output, 50, Some(source), 0)
        .expect("third data segment succeeds")
        .expect("third data segment exists");
    assert_eq!(third.payload_len(), 200);
    assert!(
        connection
            .write_next_segment(&mut output, 50, Some(source), 0)
            .expect("drained output check succeeds")
            .is_none()
    );

    let partial_ack_number = initial_sequence.wrapping_add(125);
    let partial_ack = basic_segment(
        remote_initial.wrapping_add(1),
        partial_ack_number,
        TcpFlags::ACK,
        2_000,
        &[],
    );
    let partial_ack = TcpSegment::parse(&partial_ack).expect("partial ACK parses");
    let mut receive = [];
    assert_eq!(
        connection
            .receive_segment(&partial_ack, &mut receive, 1)
            .expect("partial ACK advances"),
        (0, ReceiveStatus::empty())
    );

    let (_, duplicate_status) = connection
        .receive_segment(&partial_ack, &mut receive, 2)
        .expect("duplicate ACK is classified");
    assert!(duplicate_status.intersects(ReceiveStatus::DUPLICATE_ACK));
    let duplicate_replay = connection
        .write_next_segment(&mut output, 50, Some(source), 2)
        .expect("duplicate replay succeeds")
        .expect("duplicate replay exists");
    assert_eq!(duplicate_replay.sequence_number(), partial_ack_number);
    assert_eq!(duplicate_replay.payload_len(), 250);
    assert_eq!(&output[..250], &payload[125..375]);

    let timeout_replay = connection
        .write_next_segment(&mut output, 50, Some(source), 1 + period)
        .expect("timeout replay succeeds")
        .expect("timeout replay exists");
    assert_eq!(timeout_replay.sequence_number(), partial_ack_number);
    assert_eq!(&output[..250], &payload[125..375]);

    let mut tiny = [];
    assert_eq!(
        connection.write_next_segment(&mut tiny, 50, Some(source), 1 + period * 2),
        Err(ConnectionWriteError::PayloadBufferTooSmall)
    );
}

#[test]
fn remote_window_exhaustion_reopens_only_with_valid_ack_progress() {
    let remote_initial = 700;
    let local_initial = 900;
    let mut connection =
        open_connection(remote_initial, local_initial, 100, 300, None, (50, 15), 0);
    establish_connection(&mut connection, remote_initial, local_initial, 300, 0);

    let payload = vec![b'x'; 500];
    let initial_sequence = local_initial.wrapping_add(1);
    let source = PayloadSource::new(&payload, initial_sequence);
    let mut output = [0; 600];
    let first = connection
        .write_next_segment(&mut output, 0, Some(source), 0)
        .expect("window-sized segment succeeds")
        .expect("window-sized segment exists");
    assert_eq!(first.payload_len(), 300);
    assert!(
        connection
            .write_next_segment(&mut output, 0, Some(source), 0)
            .expect("closed window check succeeds")
            .is_none()
    );

    let future_ack = basic_segment(
        remote_initial.wrapping_add(1),
        connection.first_not_sent().wrapping_add(1),
        TcpFlags::ACK,
        300,
        &[],
    );
    let future_ack = TcpSegment::parse(&future_ack).expect("future ACK parses");
    let mut receive = [];
    let (_, status) = connection
        .receive_segment(&future_ack, &mut receive, 1)
        .expect("future ACK is classified");
    assert!(status.intersects(ReceiveStatus::INVALID_ACK));
    assert!(
        connection
            .write_next_segment(&mut output, 0, Some(source), 1)
            .expect("invalid ACK does not open window")
            .is_none()
    );

    let progress_ack_number = initial_sequence.wrapping_add(100);
    let progress_ack = basic_segment(
        remote_initial.wrapping_add(1),
        progress_ack_number,
        TcpFlags::ACK,
        300,
        &[],
    );
    let progress_ack = TcpSegment::parse(&progress_ack).expect("progress ACK parses");
    connection
        .receive_segment(&progress_ack, &mut receive, 2)
        .expect("progress ACK opens window");
    let reopened = connection
        .write_next_segment(&mut output, 0, Some(source), 2)
        .expect("reopened window write succeeds")
        .expect("reopened window output exists");
    assert_eq!(
        reopened.sequence_number(),
        initial_sequence.wrapping_add(300)
    );
    assert_eq!(reopened.payload_len(), 100);

    let stale_ack = basic_segment(
        remote_initial.wrapping_add(1),
        progress_ack_number.wrapping_sub(1),
        TcpFlags::ACK,
        300,
        &[],
    );
    let stale_ack = TcpSegment::parse(&stale_ack).expect("stale ACK parses");
    let (_, status) = connection
        .receive_segment(&stale_ack, &mut receive, 3)
        .expect("stale ACK is classified");
    assert!(status.intersects(ReceiveStatus::INVALID_ACK));
    assert_eq!(
        connection.highest_acknowledgement_received(),
        progress_ack_number
    );
}

#[test]
fn fifteenth_timeout_emits_reset_without_timestamp_regression_mutation() {
    let mut connection = open_connection(100, 200, 100, 100, None, (10, 15), 5);
    let mut output = [];
    connection
        .write_next_segment(&mut output, 0, None, 5)
        .expect("initial SYN-ACK succeeds")
        .expect("initial SYN-ACK exists");
    let original_status = connection.control_segment_or_timeout_status();
    assert!(matches!(
        connection.write_next_segment(&mut output, 0, None, 4),
        Err(ConnectionWriteError::TimestampRegression(_))
    ));
    assert_eq!(
        connection.control_segment_or_timeout_status(),
        original_status
    );

    for timeout in 1_u64..15 {
        let segment = connection
            .write_next_segment(&mut output, 0, None, 5 + timeout * 10)
            .expect("timeout write succeeds")
            .expect("timeout output exists");
        assert_eq!(segment.flags(), TcpFlags::SYNCHRONIZE | TcpFlags::ACK);
    }
    let reset = connection
        .write_next_segment(&mut output, 0, None, 5 + 15 * 10)
        .expect("fifteenth timeout write succeeds")
        .expect("fifteenth timeout emits output");
    assert!(reset.flags().intersects(TcpFlags::RESET));
    assert!(connection.is_done());
    assert_eq!(
        connection.write_next_segment(&mut output, 0, None, 5 + 15 * 10),
        Err(ConnectionWriteError::ConnectionReset)
    );
}

#[test]
fn connection_fin_and_reset_paths_match_pinned_behavior() {
    let remote_initial = 500;
    let local_initial = 900;
    let mut connection =
        open_connection(remote_initial, local_initial, 100, 100, None, (10, 15), 0);
    establish_connection(&mut connection, remote_initial, local_initial, 100, 0);
    let fin = basic_segment(
        remote_initial.wrapping_add(1),
        local_initial.wrapping_add(1),
        TcpFlags::FINISH | TcpFlags::ACK,
        100,
        &[],
    );
    let fin = TcpSegment::parse(&fin).expect("FIN parses");
    let mut receive = [];
    assert_eq!(
        connection
            .receive_segment(&fin, &mut receive, 1)
            .expect("FIN is accepted"),
        (0, ReceiveStatus::empty())
    );
    assert!(connection.fin_received());
    let mut output = [];
    let ack = connection
        .write_next_segment(&mut output, 0, None, 1)
        .expect("FIN ACK succeeds")
        .expect("FIN ACK exists");
    assert_eq!(ack.flags(), TcpFlags::ACK);
    assert_eq!(ack.acknowledgement_number(), remote_initial.wrapping_add(2));
    connection.close();
    let local_fin = connection
        .write_next_segment(&mut output, 0, None, 1)
        .expect("local FIN succeeds")
        .expect("local FIN exists");
    assert_eq!(local_fin.flags(), TcpFlags::FINISH | TcpFlags::ACK);
    assert!(connection.is_done());

    let mut reset_connection = open_connection(10, 20, 100, 100, None, (10, 15), 0);
    establish_connection(&mut reset_connection, 10, 20, 100, 0);
    let invalid_reset = basic_segment(10, 0, TcpFlags::RESET, 0, &[]);
    let invalid_reset = TcpSegment::parse(&invalid_reset).expect("invalid RST parses");
    let (_, status) = reset_connection
        .receive_segment(&invalid_reset, &mut receive, 1)
        .expect("invalid RST is classified");
    assert!(status.intersects(ReceiveStatus::INVALID_RESET));
    let valid_reset = basic_segment(11, 0, TcpFlags::RESET, 0, &[]);
    let valid_reset = TcpSegment::parse(&valid_reset).expect("valid RST parses");
    let (_, status) = reset_connection
        .receive_segment(&valid_reset, &mut receive, 2)
        .expect("valid RST is accepted");
    assert!(status.intersects(ReceiveStatus::RESET_RECEIVED));
    assert!(reset_connection.is_done());
}

#[test]
fn endpoint_holds_one_response_and_replays_from_partial_ack() {
    let remote_initial = 1_000;
    let local_initial = 5_000;
    let mut endpoint = establish_endpoint(remote_initial, local_initial, Some(100), 0);
    let callbacks = Cell::new(0_u32);

    let first_part = b"GET /one HTTP/1.1\r\n";
    let first = basic_segment(
        remote_initial.wrapping_add(1),
        local_initial.wrapping_add(1),
        TcpFlags::ACK,
        4_096,
        first_part,
    );
    let first = TcpSegment::parse(&first).expect("first request part parses");
    endpoint
        .receive_segment(&first, 1, |_| -> Result<Vec<u8>, CallbackError> {
            callbacks.set(callbacks.get() + 1);
            Ok(Vec::new())
        })
        .expect("first request part is accepted");
    assert_eq!(callbacks.get(), 0);
    assert_eq!(endpoint.buffered_request_len(), first_part.len());

    let response_one = vec![b'A'; 230];
    let second_part = b"\r\nGET /two";
    let second_sequence = remote_initial
        .wrapping_add(1)
        .wrapping_add(u32::try_from(first_part.len()).expect("test length fits"));
    let second = basic_segment(
        second_sequence,
        local_initial.wrapping_add(1),
        TcpFlags::ACK,
        4_096,
        second_part,
    );
    let second = TcpSegment::parse(&second).expect("second request part parses");
    endpoint
        .receive_segment(&second, 2, |_| -> Result<Vec<u8>, CallbackError> {
            callbacks.set(callbacks.get() + 1);
            Ok(response_one.clone())
        })
        .expect("complete first request is accepted");
    assert_eq!(callbacks.get(), 1);
    assert!(endpoint.response_pending());
    assert_eq!(endpoint.buffered_request_len(), b"GET /two".len());

    let third_part = b" HTTP/1.1\r\n\r\n";
    let third_sequence =
        second_sequence.wrapping_add(u32::try_from(second_part.len()).expect("test length fits"));
    let third = basic_segment(
        third_sequence,
        local_initial.wrapping_add(1),
        TcpFlags::ACK,
        4_096,
        third_part,
    );
    let third = TcpSegment::parse(&third).expect("pipelined request part parses");
    endpoint
        .receive_segment(&third, 3, |_| -> Result<Vec<u8>, CallbackError> {
            callbacks.set(callbacks.get() + 1);
            Ok(vec![b'X'])
        })
        .expect("pipelined request bytes are buffered");
    assert_eq!(callbacks.get(), 1);

    let mut output = [0; 200];
    let response_initial = local_initial.wrapping_add(1);
    let mut sent = Vec::new();
    for expected_len in [100, 100, 30] {
        let segment = endpoint
            .write_next_segment(&mut output, 0, 3)
            .expect("response segment succeeds")
            .expect("response segment exists");
        assert_eq!(segment.payload_len(), expected_len);
        sent.extend_from_slice(&output[..expected_len]);
    }
    assert_eq!(sent, response_one);
    assert!(matches!(
        endpoint.next_segment_status(),
        NextSegmentStatus::Timeout(_)
    ));

    let partial_ack_number = response_initial.wrapping_add(50);
    let peer_sequence =
        third_sequence.wrapping_add(u32::try_from(third_part.len()).expect("test length fits"));
    let partial_ack = basic_segment(peer_sequence, partial_ack_number, TcpFlags::ACK, 4_096, &[]);
    let partial_ack = TcpSegment::parse(&partial_ack).expect("partial response ACK parses");
    endpoint
        .receive_segment(&partial_ack, 4, no_response)
        .expect("partial response ACK succeeds");
    endpoint
        .receive_segment(&partial_ack, 5, no_response)
        .expect("duplicate response ACK succeeds");
    let replay = endpoint
        .write_next_segment(&mut output, 0, 5)
        .expect("response replay succeeds")
        .expect("response replay exists");
    assert_eq!(replay.sequence_number(), partial_ack_number);
    assert_eq!(replay.payload_len(), 100);
    assert_eq!(&output[..100], &response_one[50..150]);
    assert_eq!(callbacks.get(), 1);

    let timeout_at = 4 + MMDS_TCP_RETRANSMISSION_PERIOD_TICKS;
    let timeout_replay = endpoint
        .write_next_segment(&mut output, 0, timeout_at)
        .expect("response timeout replay succeeds")
        .expect("response timeout replay exists");
    assert_eq!(timeout_replay.sequence_number(), partial_ack_number);
    assert_eq!(timeout_replay.payload_len(), 100);
    assert_eq!(&output[..100], &response_one[50..150]);

    let full_ack = basic_segment(
        peer_sequence,
        response_initial.wrapping_add(230),
        TcpFlags::ACK,
        4_096,
        &[],
    );
    let full_ack = TcpSegment::parse(&full_ack).expect("full response ACK parses");
    let response_two = b"second response".to_vec();
    endpoint
        .receive_segment(
            &full_ack,
            timeout_at + 1,
            |_| -> Result<Vec<u8>, CallbackError> {
                callbacks.set(callbacks.get() + 1);
                Ok(response_two.clone())
            },
        )
        .expect("full ACK releases and processes pipelined request");
    assert_eq!(callbacks.get(), 2);
    assert!(endpoint.response_pending());
    assert_eq!(endpoint.buffered_request_len(), 0);
}

#[test]
fn endpoint_resets_on_callback_response_or_receive_bound_failure() {
    let remote_initial = 10;
    let local_initial = 20;
    let mut callback_failure = establish_endpoint(remote_initial, local_initial, None, 0);
    let request = b"GET / HTTP/1.1\r\n\r\n";
    let request_bytes = basic_segment(
        remote_initial.wrapping_add(1),
        local_initial.wrapping_add(1),
        TcpFlags::ACK,
        4_096,
        request,
    );
    let request_segment = TcpSegment::parse(&request_bytes).expect("request segment parses");
    assert!(matches!(
        callback_failure.receive_segment(&request_segment, 1, |_| Err(CallbackError)),
        Err(EndpointReceiveError::Callback(CallbackError))
    ));
    let mut output = [];
    let reset = callback_failure
        .write_next_segment(&mut output, 0, 1)
        .expect("callback reset write succeeds")
        .expect("callback reset exists");
    assert!(reset.flags().intersects(TcpFlags::RESET));

    let mut oversized = establish_endpoint(remote_initial, local_initial, None, 0);
    oversized.set_response_len_limit_for_test(4);
    assert!(matches!(
        oversized.receive_segment(&request_segment, 1, |_| {
            Ok::<_, CallbackError>(vec![0; 5])
        }),
        Err(EndpointReceiveError::ResponseTooLarge { len: 5, limit: 4 })
    ));
    let reset = oversized
        .write_next_segment(&mut output, 0, 1)
        .expect("oversized response reset write succeeds")
        .expect("oversized response reset exists");
    assert!(reset.flags().intersects(TcpFlags::RESET));

    let mut full = establish_endpoint(remote_initial, local_initial, None, 0);
    let full_payload = vec![b'x'; MMDS_TCP_RECEIVE_BUFFER_SIZE];
    let full_bytes = basic_segment(
        remote_initial.wrapping_add(1),
        local_initial.wrapping_add(1),
        TcpFlags::ACK,
        4_096,
        &full_payload,
    );
    let full_segment = TcpSegment::parse(&full_bytes).expect("full request segment parses");
    full.receive_segment(&full_segment, 1, no_response)
        .expect("full incomplete request is bounded");
    let reset = full
        .write_next_segment(&mut output, 0, 1)
        .expect("full request reset write succeeds")
        .expect("full request reset exists");
    assert!(reset.flags().intersects(TcpFlags::RESET));
}

#[test]
fn endpoint_eviction_and_timestamp_regression_are_safe() {
    let mut endpoint = establish_endpoint(10, 20, None, 100);
    let mut output = [];
    assert!(
        endpoint
            .write_next_segment(&mut output, 0, 200)
            .expect("idle output check succeeds")
            .is_none()
    );
    assert!(matches!(
        endpoint.is_evictable(199),
        Err(TimestampRegression { .. })
    ));
    assert!(
        !endpoint
            .is_evictable(100 + MMDS_TCP_EVICTION_THRESHOLD_TICKS)
            .expect("threshold timestamp is valid")
    );
    assert!(
        endpoint
            .is_evictable(101 + MMDS_TCP_EVICTION_THRESHOLD_TICKS)
            .expect("post-threshold timestamp is valid")
    );
    assert!(matches!(
        endpoint.is_evictable(99),
        Err(TimestampRegression { .. })
    ));
}

#[test]
fn handler_routes_handshake_and_prioritizes_bounded_resets() {
    let mut handler = MmdsTcpHandler::try_new(LOCAL_PORT).expect("handler allocation succeeds");
    assert_eq!(handler.connection_limit(), MMDS_TCP_MAX_CONNECTIONS);
    assert_eq!(handler.pending_reset_limit(), MMDS_TCP_MAX_PENDING_RESETS);
    assert_eq!(handler.connection_count(), 0);

    assert!(matches!(
        handler.receive_segment(REMOTE_ADDRESS, &[0; 5], 1, 0, no_response),
        Err(HandlerReceiveError::Segment(
            SegmentParseError::SliceTooShort { len: 5 }
        ))
    ));
    let wrong_port = segment_bytes(
        (REMOTE_PORT, LOCAL_PORT + 1),
        1,
        0,
        TcpFlags::SYNCHRONIZE,
        100,
        &[],
        &[],
    );
    assert!(matches!(
        handler.receive_segment(REMOTE_ADDRESS, &wrong_port, 10, 0, no_response),
        Err(HandlerReceiveError::InvalidPort {
            expected: LOCAL_PORT,
            actual
        }) if actual == LOCAL_PORT + 1
    ));

    let syn = syn_segment(100, 4_096, Some(1_200));
    assert_eq!(
        handler
            .receive_segment(REMOTE_ADDRESS, &syn, 500, 0, no_response)
            .expect("new SYN succeeds"),
        HandlerReceiveEvent::NewConnection
    );
    assert_eq!(handler.connection_count(), 1);
    let mut output = [];
    let syn_ack = handler
        .write_next_segment(&mut output, 0, 0)
        .expect("handler SYN-ACK write succeeds")
        .expect("handler SYN-ACK exists");
    assert_eq!(syn_ack.peer(), Peer::new(REMOTE_ADDRESS, REMOTE_PORT));
    assert_eq!(syn_ack.local_port(), LOCAL_PORT);
    assert_eq!(
        syn_ack.segment().flags(),
        TcpFlags::SYNCHRONIZE | TcpFlags::ACK
    );
    assert_eq!(syn_ack.segment().maximum_segment_size(), Some(1_200));

    let ack = basic_segment(101, 501, TcpFlags::ACK, 4_096, &[]);
    assert!(matches!(
        handler
            .receive_segment(REMOTE_ADDRESS, &ack, 0, 0, no_response)
            .expect("handler ACK succeeds"),
        HandlerReceiveEvent::ExistingConnection { status } if status.is_empty()
    ));
    assert_eq!(handler.next_segment_status(), NextSegmentStatus::Nothing);

    let unexpected = segment_bytes(
        (REMOTE_PORT + 1, LOCAL_PORT),
        1,
        7,
        TcpFlags::ACK,
        100,
        &[],
        &[],
    );
    assert_eq!(
        handler
            .receive_segment(REMOTE_ADDRESS, &unexpected, 0, 1, no_response)
            .expect("unexpected segment is classified"),
        HandlerReceiveEvent::UnexpectedSegment
    );
    assert_eq!(handler.pending_reset_count(), 1);
    let reset = handler
        .write_next_segment(&mut output, 0, 1)
        .expect("handler reset write succeeds")
        .expect("handler reset exists");
    assert!(reset.segment().flags().intersects(TcpFlags::RESET));
    assert_eq!(reset.peer().port(), REMOTE_PORT + 1);
}

#[test]
fn handler_enforces_exact_connection_eviction_and_reset_limits() {
    let mut handler = MmdsTcpHandler::try_new(LOCAL_PORT).expect("handler allocation succeeds");
    for index in 0..MMDS_TCP_MAX_CONNECTIONS {
        let source_port = 10_000_u16
            .checked_add(u16::try_from(index).expect("test index fits"))
            .expect("test source port fits");
        let syn = segment_bytes(
            (source_port, LOCAL_PORT),
            u32::try_from(index).expect("test index fits"),
            0,
            TcpFlags::SYNCHRONIZE,
            100,
            &[],
            &[],
        );
        assert_eq!(
            handler
                .receive_segment(
                    REMOTE_ADDRESS,
                    &syn,
                    100_u32
                        .checked_add(u32::try_from(index).expect("test index fits"))
                        .expect("test ISN fits"),
                    0,
                    no_response,
                )
                .expect("bounded connection opens"),
            HandlerReceiveEvent::NewConnection
        );
    }
    assert_eq!(handler.connection_count(), MMDS_TCP_MAX_CONNECTIONS);

    let at_threshold = segment_bytes(
        (20_000, LOCAL_PORT),
        99,
        0,
        TcpFlags::SYNCHRONIZE,
        100,
        &[],
        &[],
    );
    assert_eq!(
        handler
            .receive_segment(
                REMOTE_ADDRESS,
                &at_threshold,
                999,
                MMDS_TCP_EVICTION_THRESHOLD_TICKS,
                no_response,
            )
            .expect("threshold SYN is classified"),
        HandlerReceiveEvent::NewConnectionDropped
    );
    assert_eq!(handler.connection_count(), MMDS_TCP_MAX_CONNECTIONS);

    let stale = segment_bytes(
        (20_001, LOCAL_PORT),
        100,
        0,
        TcpFlags::SYNCHRONIZE,
        100,
        &[],
        &[],
    );
    assert_eq!(
        handler
            .receive_segment(
                REMOTE_ADDRESS,
                &stale,
                1_000,
                MMDS_TCP_EVICTION_THRESHOLD_TICKS + 1,
                no_response,
            )
            .expect("stale endpoint can be replaced"),
        HandlerReceiveEvent::NewConnectionReplacing
    );
    assert_eq!(handler.connection_count(), MMDS_TCP_MAX_CONNECTIONS);
    assert_eq!(handler.pending_reset_count(), 2);

    let mut reset_handler =
        MmdsTcpHandler::try_new(LOCAL_PORT).expect("reset handler allocation succeeds");
    for index in 0..=MMDS_TCP_MAX_PENDING_RESETS {
        let source_port = 30_000_u16
            .checked_add(u16::try_from(index).expect("test index fits"))
            .expect("test source port fits");
        let unexpected = segment_bytes(
            (source_port, LOCAL_PORT),
            u32::try_from(index).expect("test index fits"),
            7,
            TcpFlags::ACK,
            100,
            &[],
            &[],
        );
        assert_eq!(
            reset_handler
                .receive_segment(
                    REMOTE_ADDRESS,
                    &unexpected,
                    0,
                    u64::try_from(index).expect("test index fits"),
                    no_response,
                )
                .expect("unexpected segment is classified"),
            HandlerReceiveEvent::UnexpectedSegment
        );
    }
    assert_eq!(
        reset_handler.pending_reset_count(),
        MMDS_TCP_MAX_PENDING_RESETS
    );
}

#[test]
fn handler_time_and_allocation_failures_do_not_mutate_queues() {
    assert!(MmdsTcpHandler::try_new_for_test(LOCAL_PORT, usize::MAX, 1).is_err());

    let mut handler = MmdsTcpHandler::try_new(LOCAL_PORT).expect("handler allocation succeeds");
    let unexpected = basic_segment(1, 2, TcpFlags::ACK, 100, &[]);
    handler
        .receive_segment(REMOTE_ADDRESS, &unexpected, 0, 10, no_response)
        .expect("first timestamp succeeds");
    assert_eq!(handler.pending_reset_count(), 1);
    assert!(matches!(
        handler.receive_segment(REMOTE_ADDRESS, &unexpected, 0, 9, no_response),
        Err(HandlerReceiveError::TimestampRegression(_))
    ));
    assert_eq!(handler.pending_reset_count(), 1);
    let mut output = [];
    assert!(matches!(
        handler.write_next_segment(&mut output, 0, 9),
        Err(HandlerWriteError::TimestampRegression(_))
    ));
    assert_eq!(handler.pending_reset_count(), 1);
}
