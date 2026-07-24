use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use bangbang_pager::{
    PageAccess, PagerFrameKind, PagerPeerState, PagerRegionId, PagerSessionId, PagerTransport,
    PeerSession,
};
use bangbang_runtime::memory::GuestAddress;
use bangbang_runtime::snapshot_artifact::{
    LoadedSnapshotArtifacts, SnapshotArtifactPaths, load_snapshot_artifacts,
};
use bangbang_runtime::snapshot_memory::SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES;

const PAGER_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotPagerTermination {
    Active,
    Shutdown,
    Cancelled,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotPagerRequest {
    pub region: PagerRegionId,
    pub offset: u64,
    pub access: PageAccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotPagerReport {
    pub page_data: u64,
    pub page_zero: u64,
    pub removals: u64,
    pub requests: Vec<SnapshotPagerRequest>,
    pub termination: SnapshotPagerTermination,
}

pub struct SnapshotPagerServer {
    worker: Option<JoinHandle<()>>,
    report: Arc<Mutex<SnapshotPagerReport>>,
}

impl SnapshotPagerServer {
    pub fn start(socket: &Path, state: &Path, memory: &Path) -> Self {
        let paths = SnapshotArtifactPaths::new(state, memory);
        let artifacts =
            load_snapshot_artifacts(&paths).expect("pager-owned snapshot image should validate");
        let listener = UnixListener::bind(socket).expect("snapshot pager listener should bind");
        let report = Arc::new(Mutex::new(SnapshotPagerReport {
            page_data: 0,
            page_zero: 0,
            removals: 0,
            requests: Vec::new(),
            termination: SnapshotPagerTermination::Active,
        }));
        let worker_report = Arc::clone(&report);
        let worker = thread::spawn(move || {
            let (stream, _) = listener
                .accept()
                .expect("snapshot pager stream should accept");
            serve_snapshot(artifacts, stream, &worker_report);
        });
        Self {
            worker: Some(worker),
            report,
        }
    }

    pub fn snapshot(&self) -> SnapshotPagerReport {
        self.report
            .lock()
            .expect("snapshot pager report should not be poisoned")
            .clone()
    }

    pub fn wait(mut self) -> SnapshotPagerReport {
        self.worker
            .take()
            .expect("snapshot pager worker should exist")
            .join()
            .expect("snapshot pager worker should succeed");
        self.snapshot()
    }
}

fn serve_snapshot(
    artifacts: LoadedSnapshotArtifacts,
    stream: std::os::unix::net::UnixStream,
    report: &Mutex<SnapshotPagerReport>,
) {
    let binding = artifacts.record().memory_binding();
    let expected_session = PagerSessionId::from_bytes(binding.pager_v1_session_bytes())
        .expect("snapshot pager session should validate");
    let mut transport =
        PagerTransport::new(stream, PAGER_TIMEOUT).expect("snapshot pager transport should build");
    let mut peer = PeerSession::new();
    let hello = peer
        .receive(
            transport
                .receive()
                .expect("snapshot pager Hello should arrive"),
        )
        .expect("snapshot pager Hello should validate");
    assert_eq!(hello.kind(), PagerFrameKind::Hello);
    assert_eq!(peer.session(), Some(expected_session));
    let selected = peer
        .offered_limits()
        .expect("snapshot pager limits should be offered");
    assert_eq!(usize::from(selected.region_count()), binding.ranges().len());
    transport
        .send(
            &peer
                .hello_ack(selected)
                .expect("snapshot pager limits should select"),
        )
        .expect("snapshot pager HelloAck should send");

    let header = u64::try_from(SNAPSHOT_MEMORY_IMAGE_HEADER_BYTES)
        .expect("snapshot memory header size should fit u64");
    let mut seen = vec![false; binding.ranges().len()];
    loop {
        let frame = peer
            .receive(
                transport
                    .receive()
                    .expect("snapshot pager configuration should arrive"),
            )
            .expect("snapshot pager configuration should validate");
        match frame.kind() {
            PagerFrameKind::Region => {
                let region = frame.region().expect("region metadata should exist");
                let index =
                    usize::try_from(region.id().get() - 1).expect("region index should fit usize");
                let expected = binding
                    .ranges()
                    .get(index)
                    .expect("region ID should name one bound memory range");
                assert!(!seen[index], "snapshot pager region IDs must be unique");
                assert_eq!(region.length(), expected.range().size());
                assert_eq!(
                    region.source_offset(),
                    expected
                        .file_offset()
                        .checked_sub(header)
                        .expect("bound memory offset should follow its header")
                );
                seen[index] = true;
            }
            PagerFrameKind::Start => break,
            _ => panic!("unexpected snapshot pager configuration frame"),
        }
    }
    assert!(seen.into_iter().all(|present| present));
    transport
        .send(&peer.ready().expect("snapshot pager should become ready"))
        .expect("snapshot pager Ready should send");

    loop {
        let frame = peer
            .receive(
                transport
                    .receive()
                    .expect("snapshot pager operation should arrive"),
            )
            .expect("snapshot pager operation should validate");
        match frame.kind() {
            PagerFrameKind::PageRequest => {
                let request = frame.page_request().expect("page metadata should exist");
                let range = bound_range(binding.ranges(), request.region());
                let address = range
                    .range()
                    .start()
                    .checked_add(request.offset())
                    .expect("page guest address should remain in range");
                let mut page = vec![
                    0_u8;
                    usize::try_from(request.length())
                        .expect("page length should fit usize")
                ];
                artifacts
                    .memory()
                    .read_slice(&mut page, GuestAddress::new(address.raw_value()))
                    .expect("pager-owned memory page should read");
                let is_zero = page.iter().all(|byte| *byte == 0);
                {
                    let mut report = report
                        .lock()
                        .expect("snapshot pager report should not be poisoned");
                    if is_zero {
                        report.page_zero += 1;
                    } else {
                        report.page_data += 1;
                    }
                    report.requests.push(SnapshotPagerRequest {
                        region: request.region(),
                        offset: request.offset(),
                        access: request.access(),
                    });
                }
                if is_zero {
                    transport
                        .send(
                            &peer
                                .page_zero(request.request())
                                .expect("zero page response should build"),
                        )
                        .expect("zero page response should send");
                } else {
                    transport
                        .send(
                            &peer
                                .page_data(request.request(), page)
                                .expect("data page response should build"),
                        )
                        .expect("data page response should send");
                }
            }
            PagerFrameKind::Remove => {
                let removal = frame
                    .remove_request()
                    .expect("removal metadata should exist");
                transport
                    .send(
                        &peer
                            .removed(removal.request())
                            .expect("removal response should build"),
                    )
                    .expect("removal response should send");
                report
                    .lock()
                    .expect("snapshot pager report should not be poisoned")
                    .removals += 1;
            }
            PagerFrameKind::Shutdown => {
                transport
                    .send(
                        &peer
                            .shutdown_ack()
                            .expect("shutdown acknowledgement should build"),
                    )
                    .expect("shutdown acknowledgement should send");
                assert_eq!(peer.state(), PagerPeerState::Closed);
                report
                    .lock()
                    .expect("snapshot pager report should not be poisoned")
                    .termination = SnapshotPagerTermination::Shutdown;
                return;
            }
            PagerFrameKind::Cancel => {
                transport
                    .send(
                        &peer
                            .cancelled()
                            .expect("cancellation acknowledgement should build"),
                    )
                    .expect("cancellation acknowledgement should send");
                assert_eq!(peer.state(), PagerPeerState::Closed);
                report
                    .lock()
                    .expect("snapshot pager report should not be poisoned")
                    .termination = SnapshotPagerTermination::Cancelled;
                return;
            }
            PagerFrameKind::Terminal => {
                report
                    .lock()
                    .expect("snapshot pager report should not be poisoned")
                    .termination = SnapshotPagerTermination::Terminal;
                return;
            }
            _ => panic!("unexpected active snapshot pager frame"),
        }
    }
}

fn bound_range(
    ranges: &[bangbang_runtime::snapshot_memory::SnapshotMemoryRangeBinding],
    id: PagerRegionId,
) -> bangbang_runtime::snapshot_memory::SnapshotMemoryRangeBinding {
    let index = usize::try_from(id.get() - 1).expect("region index should fit usize");
    ranges
        .get(index)
        .copied()
        .expect("page region should name one bound memory range")
}
