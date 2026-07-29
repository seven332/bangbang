// clippy.toml allows these in #[test] bodies, but integration-test helpers are
// ordinary functions in the test crate. Keep the exception scoped to this test.
#![allow(clippy::expect_used, clippy::unwrap_used)]

#[cfg(target_os = "macos")]
#[path = "../../../tests/support/macos_virtual_block.rs"]
mod macos_virtual_block;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static HVF_LIFECYCLE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
static NEXT_HVF_TEST_FILE_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VTIMER_WRITABLE_CONTROL_MASK: u64 = 0b11;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VTIMER_TEST_OFFSET: u64 = 0x1234_5678_9abc_def0;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VTIMER_TEST_COMPARE_VALUE: u64 = 0xfedc_ba98_7654_3210;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_TEST_CNTKCTL_EL1: u64 = 3;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_TEST_CNTP_CTL_EL0: u64 = 2;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_TEST_CNTP_CVAL_EL0: u64 = 0x1234_5678;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_WRITABLE_CONTROL_MASK: u64 = 0b11;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_ISTATUS_MASK: u64 = 0b100;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_DEFINED_CONTROL_MASK: u64 = 0b111;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn is_app_sandbox_hvf_lifecycle_replay() -> bool {
    std::env::current_exe()
        .ok()
        .is_some_and(|executable| is_app_sandbox_hvf_lifecycle_executable(&executable))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn is_app_sandbox_hvf_lifecycle_executable(executable: &std::path::Path) -> bool {
    let Some(macos) = executable.parent() else {
        return false;
    };
    let Some(contents) = macos.parent() else {
        return false;
    };
    let Some(bundle) = contents.parent() else {
        return false;
    };
    executable.file_name() == Some(std::ffi::OsStr::new("hvf_lifecycle"))
        && macos.file_name() == Some(std::ffi::OsStr::new("MacOS"))
        && contents.file_name() == Some(std::ffi::OsStr::new("Contents"))
        && bundle.file_name() == Some(std::ffi::OsStr::new("BangbangHvfLifecycleSandbox.app"))
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn app_sandbox_hvf_lifecycle_replay_requires_exact_bundle_layout() {
    use std::path::Path;

    assert!(is_app_sandbox_hvf_lifecycle_executable(Path::new(
        "/tmp/BangbangHvfLifecycleSandbox.app/Contents/MacOS/hvf_lifecycle"
    )));
    assert!(!is_app_sandbox_hvf_lifecycle_executable(Path::new(
        "/tmp/BangbangHvfLifecycleSandbox.app/target/hvf_lifecycle"
    )));
    assert!(!is_app_sandbox_hvf_lifecycle_executable(Path::new(
        "/tmp/BangbangHvfLifecycleSandbox.app/Contents/MacOS/hvf_lifecycle-deadbeef"
    )));
    assert!(!is_app_sandbox_hvf_lifecycle_executable(Path::new(
        "/tmp/Other.app/Contents/MacOS/hvf_lifecycle"
    )));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod lazy_host_fault_integration {
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    use bangbang_hvf::{
        HVF_LAZY_HOST_FAULT_TERMINAL_EXIT_CODE, HvfArm64BootRegisters, HvfBackend,
        HvfLazyGuestAccess, HvfLazyGuestFaultError, HvfLazyGuestResolutionFailure,
        HvfLazyHostFaultBridge, HvfLazyPageContents, HvfLazyPageRemovalRequest, HvfLazyPageRequest,
        HvfLazyPageResolution, HvfLazyPageSource, HvfLazyPageSourceError, HvfMemoryPermissions,
        HvfVcpuRunEvent, HvfVcpuRunMemberOutcome, HvfVcpuRunStepOutcome, HvfVcpuRunnerError,
    };
    use bangbang_pager::{
        MAX_FRAME_BYTES, PageAccess, PagerLimits, PagerOperations, PagerRegionId,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::lazy_memory::{
        LazyGuestMemory, LazyGuestMemoryLimits, LazyGuestMemoryRegion, LazyPageState,
    };
    use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange};
    use bangbang_runtime::mmio::MmioDispatcher;

    use super::{HVF_LIFECYCLE_TEST_LOCK, host_page_size};

    const GUEST_BASE: u64 = 0x9000_0000;
    const SOURCE_BASE: u64 = 0x20_0000;
    const TEST_VALUE: u64 = 0x3141_5926_5358_9793;
    const TERMINAL_CHILD_ENV: &str = "BANGBANG_SIGNED_MACH_LAZY_TERMINAL_CHILD";

    enum SignedSourceReply {
        Data(Vec<u8>),
        Zero,
        Failure,
    }

    struct SignedLazySource {
        requests: Mutex<Vec<HvfLazyPageRequest>>,
        reply: SignedSourceReply,
    }

    struct BlockingSignedLazySource {
        requests: Mutex<Vec<HvfLazyPageRequest>>,
        page: Vec<u8>,
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    struct SignedRemovalRaceSource {
        page: Vec<u8>,
        removed: AtomicBool,
        entered: Mutex<Option<mpsc::Sender<()>>>,
        release: Mutex<Option<mpsc::Receiver<()>>>,
        requests: Mutex<Vec<HvfLazyPageRequest>>,
        removals: Mutex<Vec<HvfLazyPageRemovalRequest>>,
    }

    struct SignedRemovalSource {
        code: Vec<u8>,
        data: Vec<u8>,
        data_offset: u64,
        removed: AtomicBool,
        requests: Mutex<Vec<HvfLazyPageRequest>>,
        removals: Mutex<Vec<HvfLazyPageRemovalRequest>>,
    }

    impl SignedLazySource {
        fn data(page: Vec<u8>) -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                reply: SignedSourceReply::Data(page),
            }
        }

        fn zero() -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                reply: SignedSourceReply::Zero,
            }
        }

        fn failure() -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                reply: SignedSourceReply::Failure,
            }
        }
    }

    impl HvfLazyPageSource for SignedLazySource {
        fn page(
            &self,
            request: HvfLazyPageRequest,
        ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
            self.requests
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .push(request);
            match &self.reply {
                SignedSourceReply::Data(page) => Ok(HvfLazyPageContents::data(page.clone())),
                SignedSourceReply::Zero => Ok(HvfLazyPageContents::zero()),
                SignedSourceReply::Failure => Err(HvfLazyPageSourceError::failed()),
            }
        }
    }

    impl HvfLazyPageSource for BlockingSignedLazySource {
        fn page(
            &self,
            request: HvfLazyPageRequest,
        ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
            self.requests
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .push(request);
            self.entered
                .send(())
                .map_err(|_| HvfLazyPageSourceError::failed())?;
            self.release
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .recv()
                .map_err(|_| HvfLazyPageSourceError::failed())?;
            Ok(HvfLazyPageContents::data(self.page.clone()))
        }
    }

    impl HvfLazyPageSource for SignedRemovalRaceSource {
        fn page(
            &self,
            request: HvfLazyPageRequest,
        ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
            self.requests
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .push(request);
            let removed_at_entry = self.removed.load(Ordering::Acquire);
            let entered = self
                .entered
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .take();
            if let Some(entered) = entered {
                entered
                    .send(())
                    .map_err(|_| HvfLazyPageSourceError::failed())?;
                self.release
                    .lock()
                    .map_err(|_| HvfLazyPageSourceError::failed())?
                    .take()
                    .ok_or_else(HvfLazyPageSourceError::failed)?
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|_| HvfLazyPageSourceError::failed())?;
            }
            if removed_at_entry {
                Ok(HvfLazyPageContents::zero())
            } else {
                Ok(HvfLazyPageContents::data(self.page.clone()))
            }
        }

        fn remove(&self, request: HvfLazyPageRemovalRequest) -> Result<(), HvfLazyPageSourceError> {
            let expected_length =
                u64::try_from(self.page.len()).map_err(|_| HvfLazyPageSourceError::failed())?;
            if request.region().get() != 1
                || request.offset() != 0
                || request.length() != expected_length
            {
                return Err(HvfLazyPageSourceError::failed());
            }
            self.removals
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .push(request);
            self.removed.store(true, Ordering::Release);
            Ok(())
        }
    }

    impl HvfLazyPageSource for SignedRemovalSource {
        fn page(
            &self,
            request: HvfLazyPageRequest,
        ) -> Result<HvfLazyPageContents, HvfLazyPageSourceError> {
            self.requests
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .push(request);
            if request.offset() == 0 {
                Ok(HvfLazyPageContents::data(self.code.clone()))
            } else if request.offset() == self.data_offset {
                if self.removed.load(Ordering::Acquire) {
                    Ok(HvfLazyPageContents::zero())
                } else {
                    Ok(HvfLazyPageContents::data(self.data.clone()))
                }
            } else {
                Err(HvfLazyPageSourceError::failed())
            }
        }

        fn remove(&self, request: HvfLazyPageRemovalRequest) -> Result<(), HvfLazyPageSourceError> {
            if request.offset() != self.data_offset || request.length() != self.data_offset {
                return Err(HvfLazyPageSourceError::failed());
            }
            self.removals
                .lock()
                .map_err(|_| HvfLazyPageSourceError::failed())?
                .push(request);
            self.removed.store(true, Ordering::Release);
            Ok(())
        }
    }

    fn lazy_memory(page_count: u64) -> Arc<LazyGuestMemory> {
        lazy_memory_at(GUEST_BASE, page_count)
    }

    fn lazy_memory_at(guest_base: u64, page_count: u64) -> Arc<LazyGuestMemory> {
        let page_size = u32::try_from(host_page_size().expect("host page size should be valid"))
            .expect("host page size should fit u32");
        let pager = PagerLimits::new(
            page_size,
            1,
            2,
            u32::try_from(MAX_FRAME_BYTES).expect("maximum frame size should fit u32"),
            PagerOperations::v1(),
        )
        .expect("signed pager limits should validate");
        let limits = LazyGuestMemoryLimits::new(pager, page_count, 8)
            .expect("signed lazy-memory limits should validate");
        let region_size = u64::from(page_size)
            .checked_mul(page_count)
            .expect("signed lazy region size should fit");
        let range = GuestMemoryRange::new(GuestAddress::new(guest_base), region_size)
            .expect("signed guest range should validate");
        let region = LazyGuestMemoryRegion::new(
            PagerRegionId::new(1).expect("signed region id should validate"),
            range,
            SOURCE_BASE,
            page_size,
        )
        .expect("signed lazy-memory region should validate");
        Arc::new(
            LazyGuestMemory::new(limits, vec![region])
                .expect("signed lazy memory should construct"),
        )
    }

    struct AnonymousTestPage {
        pointer: NonNull<c_void>,
        length: usize,
    }

    impl AnonymousTestPage {
        fn new(length: usize) -> Self {
            // SAFETY: the arguments request one private anonymous mapping. The
            // returned pointer is checked before this owner retains it.
            let pointer = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    length,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_ANON | libc::MAP_PRIVATE,
                    -1,
                    0,
                )
            };
            assert_ne!(
                pointer,
                libc::MAP_FAILED,
                "anonymous forwarding target should map"
            );
            Self {
                pointer: NonNull::new(pointer).expect("successful mmap should be non-null"),
                length,
            }
        }

        fn as_ptr(&self) -> *mut c_void {
            self.pointer.as_ptr()
        }

        fn protect_none(&self) {
            // SAFETY: this owner retains the complete page-aligned mapping for
            // the exact length supplied to mmap.
            let status =
                unsafe { libc::mprotect(self.pointer.as_ptr(), self.length, libc::PROT_NONE) };
            assert_eq!(status, 0, "forwarding target should become inaccessible");
        }
    }

    impl Drop for AnonymousTestPage {
        fn drop(&mut self) {
            // SAFETY: this is the one retained mmap allocation and Drop runs
            // after the test handler has stopped using its address.
            let _ = unsafe { libc::munmap(self.pointer.as_ptr(), self.length) };
        }
    }

    unsafe extern "C" {
        fn bangbang_mach_test_handler_install(
            target: *mut c_void,
            target_size: usize,
            output: *mut *mut c_void,
        ) -> i32;
        fn bangbang_mach_test_handler_reinstall(handler: *mut c_void) -> i32;
        fn bangbang_mach_test_handler_is_current(handler: *mut c_void, current: *mut bool) -> i32;
        fn bangbang_mach_test_handler_handled_count(handler: *const c_void) -> usize;
        fn bangbang_mach_test_handler_shutdown(handler: *mut c_void, restored: *mut bool) -> i32;
    }

    struct MachTestHandler {
        raw: Option<NonNull<c_void>>,
    }

    impl MachTestHandler {
        fn install(target: &AnonymousTestPage) -> Self {
            let mut output = std::ptr::null_mut();
            // SAFETY: the target outlives this owner, and output is a writable
            // opaque-owner slot. The native helper retains no Rust reference.
            let status = unsafe {
                bangbang_mach_test_handler_install(target.as_ptr(), target.length, &mut output)
            };
            assert_eq!(status, 0, "test exception handler should install");
            Self {
                raw: Some(
                    NonNull::new(output)
                        .expect("successful test-handler install should return an owner"),
                ),
            }
        }

        fn reinstall(&self) {
            let raw = self.raw.expect("test handler should be live");
            // SAFETY: raw names the live native owner retained by this value.
            let status = unsafe { bangbang_mach_test_handler_reinstall(raw.as_ptr()) };
            assert_eq!(status, 0, "test exception handler should reinstall");
        }

        fn is_current(&self) -> bool {
            let raw = self.raw.expect("test handler should be live");
            let mut current = false;
            // SAFETY: raw is live and current is a writable out-parameter.
            let status =
                unsafe { bangbang_mach_test_handler_is_current(raw.as_ptr(), &mut current) };
            assert_eq!(status, 0, "current exception owner should query");
            current
        }

        fn handled_count(&self) -> usize {
            let raw = self.raw.expect("test handler should be live");
            // SAFETY: raw is retained for this read-only count query.
            unsafe { bangbang_mach_test_handler_handled_count(raw.as_ptr()) }
        }

        fn shutdown(&mut self) -> bool {
            let raw = self.raw.expect("test handler should be live");
            let mut restored = false;
            // SAFETY: raw is uniquely shut down here and restored is writable.
            let status =
                unsafe { bangbang_mach_test_handler_shutdown(raw.as_ptr(), &mut restored) };
            assert_eq!(status, 0, "test exception handler should shut down");
            self.raw = None;
            restored
        }
    }

    impl Drop for MachTestHandler {
        fn drop(&mut self) {
            let Some(raw) = self.raw else {
                return;
            };
            let mut restored = false;
            // SAFETY: this best-effort cleanup owns the remaining native
            // handler, and the out-parameter remains valid for the call.
            let status =
                unsafe { bangbang_mach_test_handler_shutdown(raw.as_ptr(), &mut restored) };
            if status == 0 {
                self.raw = None;
            }
        }
    }

    #[test]
    fn task_local_lazy_fault_bridge_populates_real_host_accesses_and_repeats() {
        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let page_size = usize::try_from(host_page_size().expect("host page size should be valid"))
            .expect("host page size should fit usize");

        for iteration in 0..2_u64 {
            let memory = lazy_memory(4);
            let pointer = memory.mapping_regions()[0]
                .host_address()
                .as_ptr()
                .cast::<u8>();
            let mut page = vec![0_u8; page_size];
            page[..std::mem::size_of::<u64>()]
                .copy_from_slice(&(TEST_VALUE + iteration).to_ne_bytes());
            let source = Arc::new(SignedLazySource::data(page));
            let bridge = HvfLazyHostFaultBridge::install(
                Arc::clone(&memory),
                Arc::<SignedLazySource>::clone(&source),
            )
            .expect("signed lazy host-fault bridge should install");

            // SAFETY: the bridge owns this retained first page and repairs its
            // read protection fault before the instruction retries.
            let read_value = unsafe { std::ptr::read_volatile(pointer.cast::<u64>()) };
            assert_eq!(read_value, TEST_VALUE + iteration);

            let written = 0xa5a5_5a5a_d1d1_e2e2_u64 + iteration;
            // SAFETY: the second retained page is valid for one u64 and the
            // bridge resolves its write-first fault before retry.
            unsafe {
                std::ptr::write_volatile(pointer.add(page_size).cast::<u64>(), written);
            }

            // SAFETY: the third retained page is aligned for AtomicU64. Its
            // load and later store permissions are mediated before retry.
            let atomic_old = unsafe {
                (&*pointer.add(page_size * 2).cast::<AtomicU64>()).fetch_add(1, Ordering::SeqCst)
            };
            assert_eq!(atomic_old, TEST_VALUE + iteration);

            let raw = (0x8877_6655_4433_2211_u64 + iteration).to_ne_bytes();
            // SAFETY: the fourth retained page has room for the complete raw
            // value and is repaired before the raw-pointer store retries.
            unsafe {
                std::ptr::copy_nonoverlapping(raw.as_ptr(), pointer.add(page_size * 3), raw.len());
            }

            let region_id = PagerRegionId::new(1).expect("signed region id should validate");
            for index in 0..4_u64 {
                assert_eq!(
                    memory
                        .page_state(
                            region_id,
                            u64::try_from(page_size).expect("page size should fit u64") * index,
                        )
                        .expect("signed lazy page state should resolve"),
                    LazyPageState::Present
                );
            }

            let requests = source
                .requests
                .lock()
                .expect("signed request log should not be poisoned");
            assert_eq!(requests.len(), 4);
            assert_eq!(
                requests
                    .iter()
                    .map(|request| request.access())
                    .collect::<Vec<_>>(),
                [
                    PageAccess::Read,
                    PageAccess::Write,
                    PageAccess::Read,
                    PageAccess::Write,
                ]
            );
            assert_eq!(
                requests
                    .iter()
                    .map(|request| request.offset())
                    .collect::<Vec<_>>(),
                (0..4_u64)
                    .map(|index| {
                        u64::try_from(page_size).expect("page size should fit u64") * index
                    })
                    .collect::<Vec<_>>()
            );
            assert!(requests.iter().all(|request| {
                request.source_offset() == SOURCE_BASE + request.offset()
                    && request.length()
                        == u32::try_from(page_size).expect("page size should fit u32")
            }));
            drop(requests);

            assert!(
                bridge
                    .shutdown()
                    .expect("signed lazy bridge should shut down")
                    .prior_handler_restored()
            );
            // SAFETY: shutdown restores the retained mapping to read/write.
            unsafe {
                assert_eq!(
                    std::ptr::read_volatile(pointer.add(page_size).cast::<u64>()),
                    written
                );
            }
        }
    }

    #[test]
    fn task_local_lazy_fault_bridge_removal_generations_refault_zero_before_and_during_population()
    {
        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let page_size = usize::try_from(host_page_size().expect("host page size should be valid"))
            .expect("host page size should fit usize");
        let page_size_u64 = u64::try_from(page_size).expect("host page size should fit u64");
        let region = PagerRegionId::new(1).expect("signed region id should validate");
        let mut page = vec![0_u8; page_size];
        page[..std::mem::size_of::<u64>()].copy_from_slice(&TEST_VALUE.to_ne_bytes());

        {
            let source = Arc::new(SignedRemovalRaceSource {
                page: page.clone(),
                removed: AtomicBool::new(false),
                entered: Mutex::new(None),
                release: Mutex::new(None),
                requests: Mutex::new(Vec::new()),
                removals: Mutex::new(Vec::new()),
            });
            let memory = lazy_memory(1);
            let pointer = memory.mapping_regions()[0]
                .host_address()
                .as_ptr()
                .cast::<u64>();
            let bridge = HvfLazyHostFaultBridge::install(
                Arc::clone(&memory),
                Arc::<SignedRemovalRaceSource>::clone(&source),
            )
            .expect("signed pre-population removal bridge should install");

            let removed = bridge
                .remove_pages(region, 0, page_size_u64)
                .expect("signed removal before population should commit");
            assert_eq!(
                bridge
                    .resolver()
                    .resolve_guest_address(GuestAddress::new(GUEST_BASE), PageAccess::Read)
                    .expect("signed post-removal population should resolve"),
                HvfLazyPageResolution::Populated
            );
            // SAFETY: the post-removal generation committed a zero page and
            // reopened this retained mapping for host reads.
            assert_eq!(unsafe { std::ptr::read_volatile(pointer) }, 0);
            let requests = source.requests.lock().expect("request log should lock");
            let removals = source.removals.lock().expect("removal log should lock");
            assert_eq!(requests.len(), 1);
            assert_eq!(removals.len(), 1);
            assert!(removed.generation().get() < requests[0].generation().get());
            drop(requests);
            drop(removals);
            assert_eq!(
                memory
                    .waiter_count()
                    .expect("signed pre-population waiter count should resolve"),
                0
            );
            assert!(
                bridge
                    .shutdown()
                    .expect("signed pre-population bridge should shut down")
                    .prior_handler_restored()
            );
        }

        {
            let (entered_sender, entered_receiver) = mpsc::channel();
            let (release_sender, release_receiver) = mpsc::channel();
            let source = Arc::new(SignedRemovalRaceSource {
                page,
                removed: AtomicBool::new(false),
                entered: Mutex::new(Some(entered_sender)),
                release: Mutex::new(Some(release_receiver)),
                requests: Mutex::new(Vec::new()),
                removals: Mutex::new(Vec::new()),
            });
            let memory = lazy_memory(1);
            let pointer = memory.mapping_regions()[0]
                .host_address()
                .as_ptr()
                .cast::<u64>();
            let bridge = HvfLazyHostFaultBridge::install(
                Arc::clone(&memory),
                Arc::<SignedRemovalRaceSource>::clone(&source),
            )
            .expect("signed in-flight removal bridge should install");
            let resolver = bridge.resolver();
            let population = std::thread::spawn(move || {
                resolver.resolve_guest_address(GuestAddress::new(GUEST_BASE), PageAccess::Read)
            });
            entered_receiver
                .recv_timeout(Duration::from_secs(5))
                .expect("signed population should block in the old generation");

            let removed = bridge
                .remove_pages(region, 0, page_size_u64)
                .expect("signed removal should supersede the in-flight generation");
            release_sender
                .send(())
                .expect("signed stale page response should be released");
            assert_eq!(
                population
                    .join()
                    .expect("signed population thread should join")
                    .expect("signed population should retry under the new generation"),
                HvfLazyPageResolution::Populated
            );
            // SAFETY: the stale data response was discarded and the retried
            // generation committed zero before reopening this mapping.
            assert_eq!(unsafe { std::ptr::read_volatile(pointer) }, 0);
            let requests = source.requests.lock().expect("request log should lock");
            let removals = source.removals.lock().expect("removal log should lock");
            assert_eq!(requests.len(), 2);
            assert_eq!(removals.len(), 1);
            assert!(
                requests[0].generation().get() < removed.generation().get()
                    && removed.generation().get() < requests[1].generation().get()
            );
            drop(requests);
            drop(removals);
            assert_eq!(
                memory
                    .waiter_count()
                    .expect("signed in-flight waiter count should resolve"),
                0
            );
            assert!(
                bridge
                    .shutdown()
                    .expect("signed in-flight removal bridge should shut down")
                    .prior_handler_restored()
            );
        }
    }

    #[test]
    fn hvf_lazy_guest_faults_populate_execute_read_and_write_before_retry() {
        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let page_size = usize::try_from(host_page_size().expect("host page size should be valid"))
            .expect("host page size should fit usize");
        let page_size_u64 = u64::try_from(page_size).expect("host page size should fit u64");
        let guest_base = 0x10_0000_u64;
        let page_addend = u32::try_from(page_size / 0x1000)
            .expect("host page increment should fit AArch64 ADD immediate");
        assert!(page_addend > 0 && page_addend <= 0xfff);
        let add_one_page = 0x9140_0000_u32 | (page_addend << 10);

        for _ in 0..2 {
            let memory = lazy_memory_at(guest_base, 3);
            let pointer = memory.mapping_regions()[0]
                .host_address()
                .as_ptr()
                .cast::<u8>();
            let instructions = [
                0xd280_0000_u32,
                0xf2a0_0200,
                add_one_page,
                0xb940_0001,
                add_one_page,
                0xb900_0001,
                0xd280_0000,
                0xd400_0002,
            ];
            let mut page = vec![0_u8; page_size];
            for (index, instruction) in instructions.iter().enumerate() {
                let start = index * std::mem::size_of::<u32>();
                page[start..start + std::mem::size_of::<u32>()]
                    .copy_from_slice(&instruction.to_le_bytes());
            }
            let source = Arc::new(SignedLazySource::data(page));
            let bridge = HvfLazyHostFaultBridge::install(
                Arc::clone(&memory),
                Arc::<SignedLazySource>::clone(&source),
            )
            .expect("signed lazy host bridge should install");
            let consumer = bridge
                .into_guest_memory_consumer()
                .expect("signed lazy consumer should claim once");

            let mut backend = HvfBackend::new();
            backend.create_vm().expect("HVF VM should be created");
            backend
                .map_lazy_guest_memory_with_consumer(consumer, HvfMemoryPermissions::GUEST_RAM)
                .expect("lazy guest memory should map with zero stage-two permission");
            let runner = backend
                .start_vcpu_runner()
                .expect("lazy-aware vCPU runner should start");
            runner
                .configure_arm64_boot_registers(HvfArm64BootRegisters {
                    kernel_entry: GuestAddress::new(guest_base),
                    fdt_address: GuestAddress::new(guest_base + page_size_u64),
                })
                .expect("lazy guest boot registers should configure");
            let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));

            for expected_access in [
                HvfLazyGuestAccess::Execute,
                HvfLazyGuestAccess::Read,
                HvfLazyGuestAccess::Write,
            ] {
                let outcome = runner
                    .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                    .expect("lazy guest exit should resolve");
                let HvfVcpuRunStepOutcome::LazyPage { fault } = outcome else {
                    panic!("first execute/read/write access should return a lazy-page outcome");
                };
                assert_eq!(fault.fault().access(), expected_access);
                assert_eq!(fault.populated_pages(), 1);
                assert_eq!(fault.permission_changes(), 1);
                assert!(!fault.stale_exit());
            }

            assert!(matches!(
                runner
                    .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                    .expect("populated guest should reach HVC"),
                HvfVcpuRunStepOutcome::Hvc { function_id: 0, .. }
            ));
            // SAFETY: write-first resolution committed this complete retained
            // page and opened host read/write before stage-two READ|WRITE.
            let written =
                unsafe { std::ptr::read_volatile(pointer.add(page_size * 2).cast::<u32>()) };
            assert_eq!(written, instructions[0]);

            let requests = source
                .requests
                .lock()
                .expect("signed request log should not be poisoned");
            assert_eq!(requests.len(), 3);
            assert_eq!(
                requests
                    .iter()
                    .map(|request| (request.offset(), request.access()))
                    .collect::<Vec<_>>(),
                vec![
                    (0, PageAccess::Read),
                    (page_size_u64, PageAccess::Read),
                    (page_size_u64 * 2, PageAccess::Write),
                ]
            );
            drop(requests);

            runner.shutdown().expect("lazy vCPU runner should stop");
            std::mem::drop(runner);
            backend
                .unmap_guest_memory()
                .expect("lazy guest mapping should unmap");
            backend.destroy_vm().expect("HVF VM should be destroyed");
        }
    }

    #[test]
    fn hvf_lazy_guest_removal_revokes_stage_two_and_refaults_zero() {
        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let page_size = usize::try_from(host_page_size().expect("host page size should be valid"))
            .expect("host page size should fit usize");
        let page_size_u64 = u64::try_from(page_size).expect("host page size should fit u64");
        let guest_base = 0x10_0000_u64;
        let page_addend = u32::try_from(page_size / 0x1000)
            .expect("host page increment should fit AArch64 ADD immediate");
        assert!(page_addend > 0 && page_addend <= 0xfff);
        let add_one_page = 0x9140_0000_u32 | (page_addend << 10);
        let instructions = [
            0xd280_0000_u32,
            0xf2a0_0200,
            add_one_page,
            0xb940_0001,
            0xd280_0000,
            0xd400_0002,
        ];
        let mut code = vec![0_u8; page_size];
        for (index, instruction) in instructions.iter().enumerate() {
            let start = index * std::mem::size_of::<u32>();
            code[start..start + std::mem::size_of::<u32>()]
                .copy_from_slice(&instruction.to_le_bytes());
        }
        let mut data = vec![0_u8; page_size];
        data[..std::mem::size_of::<u64>()].copy_from_slice(&TEST_VALUE.to_ne_bytes());
        let source = Arc::new(SignedRemovalSource {
            code,
            data,
            data_offset: page_size_u64,
            removed: AtomicBool::new(false),
            requests: Mutex::new(Vec::new()),
            removals: Mutex::new(Vec::new()),
        });
        let memory = lazy_memory_at(guest_base, 2);
        let pointer = memory.mapping_regions()[0]
            .host_address()
            .as_ptr()
            .cast::<u8>();
        let bridge = HvfLazyHostFaultBridge::install(
            Arc::clone(&memory),
            Arc::<SignedRemovalSource>::clone(&source),
        )
        .expect("signed lazy host bridge should install");
        let resolver = bridge.resolver();
        let consumer = bridge
            .into_guest_memory_consumer()
            .expect("signed removal consumer should claim once");
        let mut backend = HvfBackend::new();
        backend.create_vm().expect("HVF VM should be created");
        backend
            .map_lazy_guest_memory_with_consumer(consumer, HvfMemoryPermissions::GUEST_RAM)
            .expect("lazy guest memory should map with zero stage-two permission");
        let runner = backend
            .start_vcpu_runner()
            .expect("lazy-aware vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: GuestAddress::new(guest_base),
                fdt_address: GuestAddress::new(guest_base),
            })
            .expect("lazy guest boot registers should configure");
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));

        assert!(matches!(
            runner
                .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                .expect("instruction page should resolve"),
            HvfVcpuRunStepOutcome::LazyPage { fault }
                if fault.fault().access() == HvfLazyGuestAccess::Execute
        ));
        assert!(matches!(
            runner
                .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                .expect("data page should resolve"),
            HvfVcpuRunStepOutcome::LazyPage { fault }
                if fault.fault().access() == HvfLazyGuestAccess::Read
                    && fault.populated_pages() == 1
                    && fault.permission_changes() == 1
        ));
        // SAFETY: the guest resolver committed and opened the exact data page.
        let initial = unsafe { std::ptr::read_volatile(pointer.add(page_size).cast::<u64>()) };
        assert_eq!(initial, TEST_VALUE);

        let region = PagerRegionId::new(1).expect("signed region id should validate");
        let removed = resolver
            .remove_pages(region, page_size_u64, page_size_u64)
            .expect("signed removal should revoke both permission planes");
        assert_eq!(
            memory
                .page_state(region, page_size_u64)
                .expect("removed page state should resolve"),
            LazyPageState::Absent
        );

        let refault = runner
            .run_once_and_handle_mmio(Arc::clone(&dispatcher))
            .expect("removed guest data should fault again");
        assert!(matches!(
            refault,
            HvfVcpuRunStepOutcome::LazyPage { fault }
                if fault.fault().access() == HvfLazyGuestAccess::Read
                    && fault.populated_pages() == 1
                    && fault.permission_changes() == 1
        ));
        assert!(matches!(
            runner
                .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                .expect("zero-backed guest should reach HVC"),
            HvfVcpuRunStepOutcome::Hvc { .. }
        ));
        // SAFETY: the guest refault committed zero and reopened host reads.
        let refaulted = unsafe { std::ptr::read_volatile(pointer.add(page_size).cast::<u64>()) };
        assert_eq!(refaulted, 0);

        let requests = source.requests.lock().expect("request log should lock");
        let removals = source.removals.lock().expect("removal log should lock");
        assert_eq!(requests.len(), 3);
        assert_eq!(removals.len(), 1);
        assert_eq!(requests[1].offset(), page_size_u64);
        assert_eq!(requests[2].offset(), page_size_u64);
        assert!(
            requests[1].generation().get() < removed.generation().get()
                && removed.generation().get() < requests[2].generation().get()
        );
        drop(requests);
        drop(removals);

        runner.shutdown().expect("lazy vCPU runner should stop");
        std::mem::drop(runner);
        backend
            .unmap_guest_memory()
            .expect("lazy guest mapping should unmap");
        backend.destroy_vm().expect("HVF VM should be destroyed");
    }

    #[test]
    fn hvf_lazy_guest_two_vcpus_coalesce_one_signed_page_request() {
        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let page_size = usize::try_from(host_page_size().expect("host page size should be valid"))
            .expect("host page size should fit usize");
        let guest_base = 0x10_0000_u64;
        let memory = lazy_memory_at(guest_base, 1);
        let mut page = vec![0_u8; page_size];
        page[..std::mem::size_of::<u32>()].copy_from_slice(&0xd400_0002_u32.to_le_bytes());
        let (entered_sender, entered_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let source = Arc::new(BlockingSignedLazySource {
            requests: Mutex::new(Vec::new()),
            page,
            entered: entered_sender,
            release: Mutex::new(release_receiver),
        });
        let bridge = HvfLazyHostFaultBridge::install(
            Arc::clone(&memory),
            Arc::<BlockingSignedLazySource>::clone(&source),
        )
        .expect("signed lazy host bridge should install");
        let mut backend = HvfBackend::new();
        backend.create_vm().expect("HVF VM should be created");
        backend
            .map_lazy_guest_memory(bridge.resolver(), HvfMemoryPermissions::GUEST_RAM)
            .expect("lazy guest memory should map");
        backend
            .create_gic()
            .expect("GIC should precede a lazy two-vCPU topology");
        let topology = backend
            .start_vcpu_topology(2)
            .expect("host should support a lazy two-vCPU topology");
        let mut coordinator = topology
            .into_run_coordinator(Arc::new(Mutex::new(MmioDispatcher::new())), &[0, 1])
            .expect("lazy two-vCPU coordinator should start");
        for index in 0..2 {
            coordinator
                .configure_arm64_boot_registers(
                    index,
                    HvfArm64BootRegisters {
                        kernel_entry: GuestAddress::new(guest_base),
                        fdt_address: GuestAddress::new(guest_base),
                    },
                )
                .expect("lazy topology boot registers should configure");
        }
        assert_eq!(coordinator.dispatch_online(), Ok(2));
        entered_receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("one vCPU should enter the page source");
        let deadline = Instant::now() + Duration::from_secs(5);
        while memory
            .waiter_count()
            .expect("lazy waiter count should resolve")
            != 1
        {
            assert!(
                Instant::now() < deadline,
                "the peer vCPU did not join the in-flight page request"
            );
            std::thread::yield_now();
        }
        release_sender
            .send(())
            .expect("the signed page source should be released");

        let mut populated_pages = 0_usize;
        let mut permission_changes = 0_usize;
        let mut stale_exits = 0_usize;
        let mut members = Vec::new();
        for _ in 0..2 {
            let event = coordinator
                .receive_event()
                .expect("each lazy topology member should complete");
            let HvfVcpuRunEvent::Member(result) = event else {
                panic!("lazy peer faults should be nonterminal member events: {event:?}");
            };
            members.push(result.index());
            let Ok(HvfVcpuRunMemberOutcome::Handled(HvfVcpuRunStepOutcome::LazyPage { fault })) =
                result.result()
            else {
                panic!("lazy peer should report a handled page fault: {result:?}");
            };
            assert_eq!(fault.fault().access(), HvfLazyGuestAccess::Execute);
            populated_pages += fault.populated_pages();
            permission_changes += fault.permission_changes();
            stale_exits += usize::from(fault.stale_exit());
        }
        members.sort_unstable();
        assert_eq!(members, [0, 1]);
        assert_eq!(populated_pages, 1);
        assert_eq!(permission_changes, 1);
        assert_eq!(stale_exits, 1);
        assert_eq!(
            source
                .requests
                .lock()
                .expect("signed request log should lock")
                .len(),
            1
        );

        coordinator
            .shutdown()
            .expect("lazy topology should shut down");
        std::mem::drop(coordinator);
        backend
            .unmap_guest_memory()
            .expect("lazy topology mapping should unmap");
        backend.destroy_vm().expect("HVF VM should be destroyed");
        bridge
            .shutdown()
            .expect("lazy topology bridge should shut down");
    }

    #[test]
    fn hvf_lazy_guest_unowned_instruction_fault_keeps_existing_error_path() {
        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let guest_base = 0x10_0000_u64;
        let unowned_entry = guest_base + 0x20_0000;
        let memory = lazy_memory_at(guest_base, 1);
        let source = Arc::new(SignedLazySource::zero());
        let bridge = HvfLazyHostFaultBridge::install(
            Arc::clone(&memory),
            Arc::<SignedLazySource>::clone(&source),
        )
        .expect("signed lazy host bridge should install");
        let mut backend = HvfBackend::new();
        backend.create_vm().expect("HVF VM should be created");
        backend
            .map_lazy_guest_memory(bridge.resolver(), HvfMemoryPermissions::GUEST_RAM)
            .expect("lazy guest memory should map");
        let runner = backend
            .start_vcpu_runner()
            .expect("lazy-aware vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: GuestAddress::new(unowned_entry),
                fdt_address: GuestAddress::new(guest_base),
            })
            .expect("unowned guest entry should configure");

        let error = runner
            .run_once_and_handle_mmio(Arc::new(Mutex::new(MmioDispatcher::new())))
            .expect_err("unowned instruction fault should retain the existing error path");
        assert!(
            !matches!(error, HvfVcpuRunnerError::LazyGuestFault(_)),
            "an unowned instruction exit must not reach the lazy handler"
        );
        assert!(
            source
                .requests
                .lock()
                .expect("signed request log should lock")
                .is_empty()
        );

        runner.shutdown().expect("unowned-fault runner should stop");
        std::mem::drop(runner);
        backend
            .unmap_guest_memory()
            .expect("unowned-fault mapping should unmap");
        backend.destroy_vm().expect("HVF VM should be destroyed");
        bridge
            .shutdown()
            .expect("unowned-fault bridge should shut down");
    }

    #[test]
    fn hvf_lazy_guest_source_failure_keeps_stage_two_closed_and_cleans_up() {
        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let memory = lazy_memory_at(0x10_0000, 1);
        let source = Arc::new(SignedLazySource::failure());
        let bridge = HvfLazyHostFaultBridge::install(
            Arc::clone(&memory),
            Arc::<SignedLazySource>::clone(&source),
        )
        .expect("signed lazy host bridge should install");
        let mut backend = HvfBackend::new();
        backend.create_vm().expect("HVF VM should be created");
        backend
            .map_lazy_guest_memory(bridge.resolver(), HvfMemoryPermissions::GUEST_RAM)
            .expect("lazy guest memory should map");
        let runner = backend
            .start_vcpu_runner()
            .expect("lazy-aware vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: GuestAddress::new(0x10_0000),
                fdt_address: GuestAddress::new(0x10_0000),
            })
            .expect("lazy guest boot registers should configure");

        assert_eq!(
            runner.run_once_and_handle_mmio(Arc::new(Mutex::new(MmioDispatcher::new()))),
            Err(HvfVcpuRunnerError::LazyGuestFault(
                HvfLazyGuestFaultError::Resolution {
                    failure: HvfLazyGuestResolutionFailure::Source,
                }
            ))
        );
        assert_eq!(
            memory
                .terminal_reason()
                .expect("terminal reason should resolve"),
            Some(bangbang_runtime::lazy_memory::LazyGuestMemoryTerminalReason::TransitionFailure)
        );
        assert_eq!(
            source
                .requests
                .lock()
                .expect("source request log should lock")
                .len(),
            1
        );

        runner.shutdown().expect("failed lazy runner should stop");
        std::mem::drop(runner);
        backend
            .unmap_guest_memory()
            .expect("failed lazy guest mapping should unmap");
        backend.destroy_vm().expect("HVF VM should be destroyed");
        bridge
            .shutdown()
            .expect("failed lazy bridge should still shut down");
    }

    #[test]
    fn hvf_lazy_guest_run_cancellation_does_not_repeat_page_work() {
        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let page_size = usize::try_from(host_page_size().expect("host page size should be valid"))
            .expect("host page size should fit usize");
        let memory = lazy_memory_at(0x10_0000, 1);
        let mut page = vec![0_u8; page_size];
        page[..std::mem::size_of::<u32>()].copy_from_slice(&0x1400_0000_u32.to_le_bytes());
        let source = Arc::new(SignedLazySource::data(page));
        let bridge = HvfLazyHostFaultBridge::install(
            Arc::clone(&memory),
            Arc::<SignedLazySource>::clone(&source),
        )
        .expect("signed lazy host bridge should install");
        let mut backend = HvfBackend::new();
        backend.create_vm().expect("HVF VM should be created");
        backend
            .map_lazy_guest_memory(bridge.resolver(), HvfMemoryPermissions::GUEST_RAM)
            .expect("lazy guest memory should map");
        let runner = backend
            .start_vcpu_runner()
            .expect("lazy-aware vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: GuestAddress::new(0x10_0000),
                fdt_address: GuestAddress::new(0x10_0000),
            })
            .expect("lazy guest boot registers should configure");
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        assert!(matches!(
            runner
                .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                .expect("instruction-first fault should resolve"),
            HvfVcpuRunStepOutcome::LazyPage { fault }
                if fault.fault().access() == HvfLazyGuestAccess::Execute
        ));

        let cancel = runner.run_cancel_handle();
        std::thread::scope(|scope| {
            let run = scope.spawn(|| runner.run_once_and_handle_mmio(Arc::clone(&dispatcher)));
            std::thread::sleep(std::time::Duration::from_millis(10));
            cancel.cancel().expect("lazy guest run should cancel");
            assert_eq!(
                run.join().expect("lazy guest run thread should join"),
                Ok(HvfVcpuRunStepOutcome::Canceled)
            );
        });
        assert_eq!(
            source
                .requests
                .lock()
                .expect("source request log should lock")
                .len(),
            1
        );

        runner.shutdown().expect("canceled lazy runner should stop");
        std::mem::drop(runner);
        backend
            .unmap_guest_memory()
            .expect("canceled lazy guest mapping should unmap");
        backend.destroy_vm().expect("HVF VM should be destroyed");
        bridge
            .shutdown()
            .expect("canceled lazy bridge should shut down");
    }

    #[test]
    fn task_local_lazy_fault_bridge_forwards_and_preserves_a_later_owner() {
        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let page_size = usize::try_from(host_page_size().expect("host page size should be valid"))
            .expect("host page size should fit usize");
        let forwarding_target = AnonymousTestPage::new(page_size);
        // SAFETY: the fresh read/write mapping is aligned and large enough for
        // one u64 initialization.
        unsafe {
            std::ptr::write_volatile(forwarding_target.as_ptr().cast::<u64>(), TEST_VALUE);
        }
        let mut prior_handler = MachTestHandler::install(&forwarding_target);
        assert!(prior_handler.is_current());

        let memory = lazy_memory(1);
        let source = Arc::new(SignedLazySource::zero());
        let bridge =
            HvfLazyHostFaultBridge::install(memory, Arc::<SignedLazySource>::clone(&source))
                .expect("bridge should capture the test prior handler");
        assert!(!prior_handler.is_current());

        forwarding_target.protect_none();
        // SAFETY: this address is intentionally outside the bridge-owned lazy
        // ranges. The bridge must forward it to the captured test handler,
        // which restores this retained mapping before the instruction retries.
        let forwarded =
            unsafe { std::ptr::read_volatile(forwarding_target.as_ptr().cast::<u64>()) };
        assert_eq!(forwarded, TEST_VALUE);
        assert_eq!(prior_handler.handled_count(), 1);
        assert!(
            source
                .requests
                .lock()
                .expect("signed request log should not be poisoned")
                .is_empty(),
            "an unowned host address must not reach the lazy coordinator"
        );

        prior_handler.reinstall();
        assert!(prior_handler.is_current());
        assert!(
            !bridge
                .shutdown()
                .expect("bridge should shut down under a later owner")
                .prior_handler_restored(),
            "bridge shutdown must preserve the handler that replaced it"
        );
        assert!(prior_handler.is_current());
        assert!(
            prior_handler.shutdown(),
            "test handler should restore the configuration captured before the bridge"
        );
    }

    #[test]
    fn task_local_lazy_fault_bridge_uses_fixed_terminal_exit_on_owned_failure() {
        if std::env::var_os(TERMINAL_CHILD_ENV).is_none() {
            let executable =
                std::env::current_exe().expect("signed lifecycle executable should resolve");
            let output = std::process::Command::new(executable)
                .args([
                    "--exact",
                    "lazy_host_fault_integration::task_local_lazy_fault_bridge_uses_fixed_terminal_exit_on_owned_failure",
                    "--nocapture",
                ])
                .env(TERMINAL_CHILD_ENV, "1")
                .output()
                .expect("signed terminal child should launch");
            assert_eq!(
                output.status.code(),
                Some(HVF_LAZY_HOST_FAULT_TERMINAL_EXIT_CODE),
                "owned failure should take the fixed terminal exit\nstdout={}\nstderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
            );
            return;
        }

        let _test_lock = HVF_LIFECYCLE_TEST_LOCK
            .lock()
            .expect("HVF lifecycle test lock should not be poisoned");
        let memory = lazy_memory(1);
        let pointer = memory.mapping_regions()[0]
            .host_address()
            .as_ptr()
            .cast::<u8>();
        let source = Arc::new(SignedLazySource::failure());
        let _bridge = HvfLazyHostFaultBridge::install(memory, source)
            .expect("terminal child bridge should install");
        // SAFETY: this retained owned page deliberately faults. The source
        // failure must terminate the process before this instruction returns.
        let _ = unsafe { std::ptr::read_volatile(pointer) };
        panic!("owned terminal fault unexpectedly returned");
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn temporary_virtual_block_fixture_preserves_rw_ro_bytes_and_exact_cleanup() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use crate::macos_virtual_block::{MacosVirtualBlock, MacosVirtualBlockAccess};
    use bangbang_runtime::block::{
        BlockDeviceControl, BlockDeviceControlError, BlockDeviceGeometry, BlockFileBacking,
        BlockFileBackingError, SnapshotBlockFileBackingError, VirtioBlockDeviceId,
    };

    #[derive(Debug)]
    struct RejectInspectControl;

    impl BlockDeviceControl for RejectInspectControl {
        fn inspect(
            &self,
            _file: &std::fs::File,
        ) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
            Err(BlockDeviceControlError::new(
                std::io::ErrorKind::PermissionDenied,
            ))
        }

        fn synchronize_cache(&self, _file: &std::fs::File) -> Result<(), BlockDeviceControlError> {
            panic!("failed inspection must not publish a backing")
        }
    }

    #[derive(Debug)]
    struct RejectSyncControl {
        geometry: BlockDeviceGeometry,
    }

    impl BlockDeviceControl for RejectSyncControl {
        fn inspect(
            &self,
            _file: &std::fs::File,
        ) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
            Ok(self.geometry)
        }

        fn synchronize_cache(&self, _file: &std::fs::File) -> Result<(), BlockDeviceControlError> {
            Err(BlockDeviceControlError::new(
                std::io::ErrorKind::PermissionDenied,
            ))
        }
    }

    #[derive(Debug)]
    struct ChangingGeometryControl {
        initial: BlockDeviceGeometry,
        changed: BlockDeviceGeometry,
        inspections: AtomicUsize,
    }

    impl BlockDeviceControl for ChangingGeometryControl {
        fn inspect(
            &self,
            _file: &std::fs::File,
        ) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
            if self.inspections.fetch_add(1, AtomicOrdering::Relaxed) == 0 {
                Ok(self.initial)
            } else {
                Ok(self.changed)
            }
        }

        fn synchronize_cache(&self, _file: &std::fs::File) -> Result<(), BlockDeviceControlError> {
            Ok(())
        }
    }

    // The wrapper already runs this test as a directly signed binary. Its App Sandbox replay
    // cannot launch the test-only `hdiutil` fixture process and is covered by #1465 instead.
    if is_app_sandbox_hvf_lifecycle_replay() {
        return;
    }
    let _guard = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut media = MacosVirtualBlock::create(MacosVirtualBlockAccess::ReadWrite)
        .expect("temporary virtual block media should create");
    let marker = b"bangbang-virtual-block";
    let device = media
        .device_path()
        .expect("attached device path should exist")
        .to_path_buf();
    let identity = media
        .identity()
        .expect("attached identity should be available");

    assert_eq!(
        media.len().expect("media length should read"),
        4 * 1024 * 1024
    );
    assert_ne!(identity.target_device(), 0);
    assert_eq!(
        u64::from(
            media
                .logical_block_size()
                .expect("logical block size should read")
        ) * media.block_count().expect("block count should read"),
        media.len().expect("media length should read")
    );
    assert!(format!("{media:?}").contains("<redacted>"));
    assert!(!format!("{media:?}").contains(device.to_string_lossy().as_ref()));
    assert!(!format!("{identity:?}").contains(&identity.target_device().to_string()));

    let logical_block_size = media
        .logical_block_size()
        .expect("logical block size should read");
    let block_count = media.block_count().expect("block count should read");
    let geometry = BlockDeviceGeometry::new(logical_block_size, block_count)
        .expect("fixture geometry should validate");
    let inspect_error = BlockFileBacking::from_file_with_block_device_control(
        media
            .open_descriptor()
            .expect("inspect-failure block descriptor should open"),
        false,
        Arc::new(RejectInspectControl),
    )
    .expect_err("control inspection failure should reject adoption");
    assert!(matches!(
        inspect_error,
        BlockFileBackingError::ReadBlockGeometry { source }
            if source.kind() == std::io::ErrorKind::PermissionDenied
    ));

    let rejecting_sync = BlockFileBacking::from_file_with_block_device_control(
        media
            .open_descriptor()
            .expect("sync-failure block descriptor should open"),
        false,
        Arc::new(RejectSyncControl { geometry }),
    )
    .expect("valid injected inspection should adopt the real block descriptor");
    assert!(matches!(
        rejecting_sync.flush(),
        Err(BlockFileBackingError::FlushBlockDevice { source })
            if source.kind() == std::io::ErrorKind::PermissionDenied
    ));
    drop(rejecting_sync);

    let changed_geometry = BlockDeviceGeometry::new(
        logical_block_size,
        block_count
            .checked_sub(1)
            .expect("fixture should have more than one logical block"),
    )
    .expect("changed fixture geometry should remain structurally valid");
    let changing = BlockFileBacking::from_file_with_block_device_control(
        media
            .open_descriptor()
            .expect("geometry-change block descriptor should open"),
        false,
        Arc::new(ChangingGeometryControl {
            initial: geometry,
            changed: changed_geometry,
            inspections: AtomicUsize::new(0),
        }),
    )
    .expect("initial injected geometry should adopt the real block descriptor");
    assert_eq!(
        changing.snapshot_identity(),
        Err(SnapshotBlockFileBackingError::InvalidMetadata)
    );
    drop(changing);

    {
        let backing = BlockFileBacking::from_file(
            media
                .open_descriptor()
                .expect("read-write block descriptor should open"),
            false,
        )
        .expect("runtime should adopt read-write block descriptor");
        assert!(backing.kind().is_block_device());
        assert_eq!(
            backing.kind().logical_block_size(),
            Some(
                media
                    .logical_block_size()
                    .expect("logical block size should read")
            )
        );
        assert_eq!(
            backing.len(),
            media.len().expect("media length should read")
        );
        assert_eq!(
            backing.device_id(),
            VirtioBlockDeviceId::from_bytes(
                format!(
                    "{}{}{}",
                    identity.device(),
                    identity.target_device(),
                    identity.inode()
                )
                .as_bytes()
            )
        );
        let backing_debug = format!("{backing:?}");
        assert!(backing_debug.contains("<redacted>"));
        assert!(!backing_debug.contains(&backing.len().to_string()));
        assert!(!backing_debug.contains(&identity.target_device().to_string()));
        backing
            .write_at(4096, marker)
            .expect("runtime block write should succeed");
        backing
            .flush()
            .expect("runtime block cache synchronization should succeed");
        let mut readback = vec![0_u8; marker.len()];
        backing
            .read_at(4096, &mut readback)
            .expect("runtime block read should succeed");
        assert_eq!(readback, marker);
        let capture_identity = backing
            .snapshot_identity()
            .expect("runtime block capture identity should revalidate");
        assert!(capture_identity.kind().is_block_device());
        assert_eq!(
            capture_identity.target_device(),
            Some(identity.target_device())
        );
        assert_eq!(
            capture_identity.block_count(),
            Some(media.block_count().expect("block count should read"))
        );
    }
    assert_eq!(
        media
            .read_at(4096, marker.len())
            .expect("read-write attachment should read marker"),
        marker
    );
    media
        .reattach(MacosVirtualBlockAccess::ReadOnly)
        .expect("media should reattach read-only");
    assert_eq!(
        media
            .read_at(4096, marker.len())
            .expect("read-only attachment should read persisted marker"),
        marker
    );
    let read_only_backing = BlockFileBacking::from_file(
        media
            .open_descriptor()
            .expect("read-only block descriptor should open"),
        true,
    )
    .expect("runtime should adopt read-only block descriptor");
    let mut readback = vec![0_u8; marker.len()];
    read_only_backing
        .read_at(4096, &mut readback)
        .expect("runtime read-only block read should succeed");
    assert_eq!(readback, marker);
    assert!(matches!(
        read_only_backing.write_at(4096, b"rejected"),
        Err(BlockFileBackingError::ReadOnlyWrite)
    ));
    drop(read_only_backing);
    assert!(media.write_at(4096, b"rejected").is_err());
    media
        .cleanup()
        .expect("temporary virtual block media should detach and clean up");
    assert!(!device.exists());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn temporary_virtual_block_traverses_async_flush_and_capture_ready_owner() {
    use std::sync::Arc;
    use std::time::Instant;

    use crate::macos_virtual_block::{MacosVirtualBlock, MacosVirtualBlockAccess};
    use bangbang_hvf::{
        HvfArm64BootSessionConfig, HvfArm64BootStorageCaptureErrorKind, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::async_executor::{
        BlockAsyncApplyOutcome, BlockAsyncCompletionDisposition, BlockAsyncDrive,
        BlockAsyncDriveGeneration, BlockAsyncExecutor, BlockAsyncOperation,
        BlockAsyncOperationKind, BlockAsyncOperationStatus, BlockAsyncRequestIdentity,
        BlockAsyncScheduleOutcome,
    };
    use bangbang_runtime::block::{
        BlockCaptureIoEngine, BlockDeviceControl, BlockDeviceControlError, BlockDeviceGeometry,
        BlockFileBacking, BlockFileBackingError, BlockMmioLayout, DriveCacheType, DriveConfigInput,
        DriveIoEngine, DriveLiveUpdateMode,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryLayout, GuestMemoryRange,
    };
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::storage_capture::{CaptureReadyStorageConfigs, StorageTransportState};
    use bangbang_runtime::vsock::VsockMmioLayout;

    #[derive(Debug)]
    struct RejectReplacementInspectControl;

    impl BlockDeviceControl for RejectReplacementInspectControl {
        fn inspect(
            &self,
            _file: &std::fs::File,
        ) -> Result<BlockDeviceGeometry, BlockDeviceControlError> {
            Err(BlockDeviceControlError::new(
                std::io::ErrorKind::PermissionDenied,
            ))
        }

        fn synchronize_cache(&self, _file: &std::fs::File) -> Result<(), BlockDeviceControlError> {
            panic!("failed replacement inspection must not publish a backing")
        }
    }

    // The wrapper already runs this test as a directly signed binary. Its App Sandbox replay
    // cannot launch the test-only `hdiutil` fixture process and is covered by #1465 instead.
    if is_app_sandbox_hvf_lifecycle_replay() {
        return;
    }
    let _guard = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let media = MacosVirtualBlock::create(MacosVirtualBlockAccess::ReadWrite)
        .expect("temporary virtual block media should create");
    let device = media
        .device_path()
        .expect("attached device path should exist")
        .to_path_buf();
    let media_identity = media
        .identity()
        .expect("attached identity should be available");
    let media_len = media.len().expect("media length should read");
    let logical_block_size = media
        .logical_block_size()
        .expect("logical block size should read");
    let block_count = media.block_count().expect("block count should read");

    let backing = Arc::new(
        BlockFileBacking::from_file(
            media
                .open_descriptor()
                .expect("async block descriptor should open"),
            false,
        )
        .expect("runtime should adopt the async block descriptor"),
    );
    let expected_device_id = backing.device_id();
    let mut executor = BlockAsyncExecutor::new().expect("production async executor should start");
    let completion_fd = executor
        .completion_fd()
        .expect("production async executor should expose a completion descriptor");
    let mut drive = BlockAsyncDrive::new(
        BlockAsyncDriveGeneration::new(1),
        Arc::clone(&backing),
        DriveCacheType::Writeback,
        executor.handle(),
    )
    .expect("block drive should bind to the production async executor");
    let layout = GuestMemoryLayout::new(vec![
        GuestMemoryRange::new(GuestAddress::new(0), 16 * 1024)
            .expect("async flush guest range should validate"),
    ])
    .expect("async flush guest layout should validate");
    let mut memory =
        GuestMemory::allocate(&layout).expect("async flush guest memory should allocate");
    let operation = drive
        .admit(BlockAsyncOperation::flush(BlockAsyncRequestIdentity::new(
            0,
            0,
            GuestAddress::new(0),
        )))
        .expect("real block flush should admit");
    assert!(matches!(
        drive
            .schedule_one(&memory)
            .expect("real block flush should schedule"),
        BlockAsyncScheduleOutcome::Submitted {
            operation: submitted,
            chunk_offset: 0,
            chunk_len: 0,
        } if submitted == operation
    ));
    let mut readiness = libc::pollfd {
        fd: completion_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: One initialized pollfd is writable for the bounded wait.
    let ready = unsafe { libc::poll(&raw mut readiness, 1, 5_000) };
    assert_eq!(
        ready, 1,
        "real block flush should complete before the deadline"
    );
    assert_ne!(readiness.revents & libc::POLLIN, 0);
    let completion = executor
        .try_recv_completion()
        .expect("production completion queue should remain connected")
        .expect("readiness should publish the real block flush completion");
    let BlockAsyncApplyOutcome::Completed(applied) = drive
        .apply_completion(
            &mut memory,
            completion,
            BlockAsyncCompletionDisposition::Apply,
        )
        .expect("real block flush completion should apply")
    else {
        panic!("real block flush should complete in one host operation");
    };
    assert_eq!(applied.kind(), BlockAsyncOperationKind::Flush);
    assert_eq!(applied.status(), BlockAsyncOperationStatus::Success);
    assert_eq!(applied.bytes_transferred(), 0);
    drop(drive);
    executor
        .shutdown()
        .expect("production async executor should stop");
    drop(executor);
    drop(backing);

    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("block-capture-ready-kernel", &image)
        .expect("block capture kernel should create");
    let root = TempFile::new_len("block-capture-ready-root", 4096)
        .expect("regular block capture root should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("block capture boot source should configure");
    controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", root.path(), true)
                .with_is_read_only(true)
                .with_io_engine(DriveIoEngine::Sync),
        ))
        .expect("regular capture root should configure");
    controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("blockdata", "blockdata", device.as_path(), false)
                .with_is_read_only(false)
                .with_cache_type(DriveCacheType::Writeback)
                .with_io_engine(DriveIoEngine::Async),
        ))
        .expect("real block data drive should configure");
    let session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut session = OwnedHvfArm64BootSession::new(&controller, session_config)
        .expect("signed block capture session should prepare");
    let configs = CaptureReadyStorageConfigs::new(controller.drive_configs().to_vec(), Vec::new());
    let retry_guard = session
        .quiesce_limiter_retry_wakeups()
        .expect("block capture retry publishers should quiesce");
    let profile_error = session
        .capture_snapshot_v2_multi_block_device_graph_at(&configs, &retry_guard, Instant::now())
        .expect_err("block-special backing must fail profile preflight");
    assert_eq!(
        profile_error.kind(),
        HvfArm64BootStorageCaptureErrorKind::ProfilePreflight
    );
    assert!(!profile_error.terminal());
    let first = session
        .capture_ready_storage_state_at(&configs, &retry_guard, Instant::now())
        .expect("real block drive should become capture-ready");
    let second = session
        .capture_ready_storage_state_at(&configs, &retry_guard, Instant::now())
        .expect("real block Async admission should reopen for a second capture");
    assert_eq!(first.block_devices().len(), 2);
    assert_eq!(
        first.block_devices()[1].config(),
        &controller.drive_configs()[1]
    );
    assert!(
        first.block_devices()[0]
            .device()
            .backing()
            .kind()
            .is_regular_file()
    );
    assert!(matches!(
        first.block_devices()[1].transport(),
        StorageTransportState::Mmio(_)
    ));
    let first_device = first.block_devices()[1].device();
    let second_device = second.block_devices()[1].device();
    let captured_backing = first_device.backing();
    assert!(captured_backing.kind().is_block_device());
    assert_eq!(
        captured_backing.target_device(),
        Some(media_identity.target_device())
    );
    assert_eq!(captured_backing.len(), media_len);
    assert_eq!(
        captured_backing.kind().logical_block_size(),
        Some(logical_block_size)
    );
    assert_eq!(captured_backing.block_count(), Some(block_count));
    assert_eq!(
        first_device.config_space().capacity_sectors(),
        media_len / 512
    );
    assert_eq!(first_device.device_id(), expected_device_id);
    assert_eq!(second_device.backing(), captured_backing);
    assert_eq!(second_device.device_id(), first_device.device_id());
    let BlockCaptureIoEngine::Async(first_async) = first_device.io_engine() else {
        panic!("real block drive should retain Async continuation state");
    };
    let BlockCaptureIoEngine::Async(second_async) = second_device.io_engine() else {
        panic!("second real block capture should retain Async continuation state");
    };
    assert_eq!(second_async.generation(), first_async.generation());
    assert!(first_async.admission_stopped());
    assert_eq!(first_async.owned_operations(), 0);
    assert_eq!(first_async.parked_host_completions(), 0);
    assert_eq!(first_async.final_completions(), 0);

    let failed_replacement = BlockFileBacking::from_file_with_block_device_control(
        media
            .open_descriptor()
            .expect("failed replacement block descriptor should open"),
        false,
        Arc::new(RejectReplacementInspectControl),
    )
    .expect_err("failed replacement inspection should reject before publication");
    assert!(matches!(
        failed_replacement,
        BlockFileBackingError::ReadBlockGeometry { source }
            if source.kind() == std::io::ErrorKind::PermissionDenied
    ));
    let after_failed_replacement = session
        .capture_ready_storage_state_at(&configs, &retry_guard, Instant::now())
        .expect("failed replacement preparation must leave the prior owner capture-ready");
    let failed_device = after_failed_replacement.block_devices()[1].device();
    assert_eq!(failed_device.backing(), captured_backing);
    assert_eq!(failed_device.device_id(), expected_device_id);
    let BlockCaptureIoEngine::Async(failed_async) = failed_device.io_engine() else {
        panic!("failed replacement must retain the prior Async engine");
    };
    assert_eq!(failed_async.generation(), second_async.generation());

    drop(retry_guard);
    let replacement_config = controller.drive_configs()[1].clone();
    let replacement_backing = BlockFileBacking::from_file(
        media
            .open_descriptor()
            .expect("successful replacement block descriptor should open"),
        false,
    )
    .expect("successful replacement block backing should prepare");
    session
        .update_live_block_device_with_opened(
            &replacement_config,
            Some(replacement_backing),
            None,
            DriveLiveUpdateMode::Replacement,
        )
        .expect("real block replacement should commit through the MMIO owner");
    let replacement_guard = session
        .quiesce_limiter_retry_wakeups()
        .expect("replacement capture retry publishers should quiesce");
    let after_successful_replacement = session
        .capture_ready_storage_state_at(&configs, &replacement_guard, Instant::now())
        .expect("successfully replaced real block drive should become capture-ready");
    let replacement_device = after_successful_replacement.block_devices()[1].device();
    assert_eq!(replacement_device.backing(), captured_backing);
    assert_eq!(replacement_device.device_id(), expected_device_id);
    assert_eq!(
        replacement_device.config_space().capacity_sectors(),
        media_len / 512
    );
    let BlockCaptureIoEngine::Async(replacement_async) = replacement_device.io_engine() else {
        panic!("successful block replacement should retain the configured Async engine");
    };
    assert_ne!(replacement_async.generation(), second_async.generation());
    assert!(replacement_async.admission_stopped());
    assert_eq!(replacement_async.owned_operations(), 0);
    drop(replacement_guard);
    session
        .shutdown()
        .expect("signed block capture session should shut down");
    drop(session);
    media
        .cleanup()
        .expect("temporary virtual block media should detach and clean up");
    assert!(!device.exists());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn mach_counter_sample() -> u64 {
    // SAFETY: `mach_absolute_time` takes no arguments and returns one monotonic sample.
    unsafe { mach_absolute_time() }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn mach_ticks_for(duration: std::time::Duration) -> Option<u64> {
    let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
    // SAFETY: `info` is a valid, exclusively borrowed output object for the call.
    assert_eq!(unsafe { mach_timebase_info(&mut info) }, 0);
    assert_ne!(info.numer, 0);
    assert_ne!(info.denom, 0);

    let nanoseconds = duration.as_nanos();
    let numerator = nanoseconds.checked_mul(u128::from(info.denom))?;
    let rounded = numerator.checked_add(u128::from(info.numer) - 1)?;
    let ticks = rounded / u128::from(info.numer);
    u64::try_from(ticks).ok()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_normalized_timer_restore_equivalent(
    source: bangbang_hvf::HvfArm64SnapshotTimerState,
    recaptured: bangbang_hvf::HvfArm64SnapshotTimerState,
) {
    assert_eq!(
        recaptured.virtual_timer_exit_masked(),
        source.virtual_timer_exit_masked()
    );
    assert_eq!(recaptured.cntkctl_el1(), source.cntkctl_el1());
    assert_eq!(recaptured.virtual_control(), source.virtual_control());
    assert_eq!(
        recaptured.virtual_compare_value(),
        source.virtual_compare_value()
    );
    assert_eq!(recaptured.physical_control(), source.physical_control());

    let virtual_elapsed = recaptured
        .virtual_count()
        .wrapping_sub(source.virtual_count());
    let physical_elapsed = source
        .physical_compare_delta()
        .wrapping_sub(recaptured.physical_compare_delta());
    assert_eq!(
        virtual_elapsed, physical_elapsed,
        "virtual count and physical comparator distance should advance by one shared host-counter interval"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_native_v2_platform_recapture_equivalent(
    source: &bangbang_hvf::HvfSnapshotV2PlatformState,
    recaptured: &bangbang_hvf::HvfSnapshotV2PlatformState,
) {
    assert_eq!(recaptured.memory().version(), source.memory().version());
    assert_eq!(recaptured.memory().extents(), source.memory().extents());
    assert_eq!(
        recaptured.memory().file_length(),
        source.memory().file_length()
    );
    assert_eq!(recaptured.machine(), source.machine());
    assert_eq!(
        recaptured.global().compatibility(),
        source.global().compatibility()
    );
    assert_eq!(recaptured.topology(), source.topology());
    assert_eq!(recaptured.vcpus().len(), source.vcpus().len());
    for (source_vcpu, recaptured_vcpu) in source.vcpus().iter().zip(recaptured.vcpus()) {
        assert_eq!(recaptured_vcpu.index(), source_vcpu.index());
        assert_eq!(recaptured_vcpu.mpidr(), source_vcpu.mpidr());
        assert_eq!(recaptured_vcpu.mandatory(), source_vcpu.mandatory());
        assert_eq!(
            recaptured_vcpu.pending_interrupts(),
            source_vcpu.pending_interrupts()
        );
        assert_eq!(recaptured_vcpu.gic_icc(), source_vcpu.gic_icc());
        assert_eq!(
            recaptured_vcpu.reviewed_optional(),
            source_vcpu.reviewed_optional()
        );
        assert_normalized_timer_restore_equivalent(*source_vcpu.timer(), *recaptured_vcpu.timer());
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn arm64_adr(pc_offset: u64, target_offset: u64, register: u8) -> u32 {
    let delta = i64::try_from(target_offset).expect("ADR target should fit")
        - i64::try_from(pc_offset).expect("ADR PC should fit");
    assert!(
        (-(1_i64 << 20)..(1_i64 << 20)).contains(&delta),
        "ADR target should fit its signed 21-bit immediate"
    );
    let immediate =
        u32::try_from(delta.rem_euclid(1_i64 << 21)).expect("ADR immediate should fit 21 bits");
    0x1000_0000
        | ((immediate & 0b11) << 29)
        | (((immediate >> 2) & 0x7_ffff) << 5)
        | u32::from(register)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn arm64_instruction_bytes(instructions: &[u32]) -> Vec<u8> {
    instructions
        .iter()
        .flat_map(|instruction| instruction.to_le_bytes())
        .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_pstate_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmePstate, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmePstate>, bangbang_hvf::HvfVcpuRunnerError> {
    use bangbang_hvf::HvfVcpuRunnerError;
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::Backend(BackendError::Unsupported(message))) => {
            assert_eq!(
                message,
                "Hypervisor.framework SME state capture requires macOS 15.2 or newer"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::Backend(BackendError::Hypervisor(message))) => {
            assert_eq!(
                message,
                "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_p_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmePRegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmePRegisterState>, bangbang_hvf::HvfVcpuRunnerError> {
    use bangbang_hvf::{HvfArm64VcpuSmePRegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmePRegisterCapture(
            HvfArm64VcpuSmePRegisterCaptureError::StreamingSveModeDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmePRegisterCapture(
            HvfArm64VcpuSmePRegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer",
                    "Hypervisor.framework SME P-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmePRegisterCapture(
            HvfArm64VcpuSmePRegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_p_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_z_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmeZRegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmeZRegisterState>, bangbang_hvf::HvfVcpuRunnerError> {
    use bangbang_hvf::{HvfArm64VcpuSmeZRegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmeZRegisterCapture(
            HvfArm64VcpuSmeZRegisterCaptureError::StreamingSveModeDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmeZRegisterCapture(
            HvfArm64VcpuSmeZRegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer",
                    "Hypervisor.framework SME Z-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmeZRegisterCapture(
            HvfArm64VcpuSmeZRegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_z_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_za_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmeZaRegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmeZaRegisterState>, bangbang_hvf::HvfVcpuRunnerError>
{
    use bangbang_hvf::{HvfArm64VcpuSmeZaRegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmeZaRegisterCapture(
            HvfArm64VcpuSmeZaRegisterCaptureError::ZaStorageDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmeZaRegisterCapture(
            HvfArm64VcpuSmeZaRegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME configuration queries require macOS 15.2 or newer",
                    "Hypervisor.framework SME ZA-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmeZaRegisterCapture(
            HvfArm64VcpuSmeZaRegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_za_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_zt0_register_capture_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64VcpuSmeZt0RegisterState, bangbang_hvf::HvfVcpuRunnerError>,
) -> Result<Option<bangbang_hvf::HvfArm64VcpuSmeZt0RegisterState>, bangbang_hvf::HvfVcpuRunnerError>
{
    use bangbang_hvf::{HvfArm64VcpuSmeZt0RegisterCaptureError, HvfVcpuRunnerError};
    use bangbang_runtime::BackendError;

    match result {
        Ok(state) => Ok(Some(state)),
        Err(HvfVcpuRunnerError::SmeZt0RegisterCapture(
            HvfArm64VcpuSmeZt0RegisterCaptureError::ZaStorageDisabled,
        )) => Ok(None),
        Err(HvfVcpuRunnerError::SmeZt0RegisterCapture(
            HvfArm64VcpuSmeZt0RegisterCaptureError::Backend(BackendError::Unsupported(message)),
        )) => {
            assert!(
                [
                    "Hypervisor.framework SME state capture requires macOS 15.2 or newer",
                    "Hypervisor.framework SME ZT0-register capture requires macOS 15.2 or newer",
                ]
                .contains(&message),
                "only a documented macOS 15.2 SME availability boundary is accepted"
            );
            Ok(None)
        }
        Err(HvfVcpuRunnerError::SmeZt0RegisterCapture(
            HvfArm64VcpuSmeZt0RegisterCaptureError::Backend(BackendError::Hypervisor(message)),
        )) => {
            assert!(
                [
                    "hv_vcpu_get_sme_state failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                    "hv_vcpu_get_sme_zt0_reg failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)",
                ]
                .contains(&message.as_str()),
                "only a documented HV_UNSUPPORTED SME availability result is accepted"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn assert_sme_configuration_supported_or_unavailable(
    result: Result<bangbang_hvf::HvfArm64SmeConfiguration, bangbang_runtime::BackendError>,
) -> Result<Option<bangbang_hvf::HvfArm64SmeConfiguration>, bangbang_runtime::BackendError> {
    use bangbang_runtime::BackendError;

    match result {
        Ok(configuration) => Ok(Some(configuration)),
        Err(BackendError::Hypervisor(message)) => {
            assert_eq!(
                message,
                "hv_sme_config_get_max_svl_bytes failed with HV_UNSUPPORTED (hv_return_t=0xfae9400f)"
            );
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const PHYSICAL_TIMER_GUEST_CODE: [u32; 9] = [
    0xd280_0060, // mov x0, #3
    0xd518_e100, // msr CNTKCTL_EL1, x0
    0xd280_0040, // mov x0, #2
    0xd51b_e220, // msr CNTP_CTL_EL0, x0
    0xd28a_cf00, // mov x0, #0x5678
    0xf2a2_4680, // movk x0, #0x1234, lsl #16
    0xd51b_e240, // msr CNTP_CVAL_EL0, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_SP_EL0: u64 = 0x1000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_SP_EL1: u64 = 0x2000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_ELR_EL1: u64 = 0x3000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_TEST_SPSR_EL1: u64 = 0x3c5;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const CORE_SYSTEM_REGISTER_GUEST_CODE: [u32; 9] = [
    0xd282_0000, // mov x0, #0x1000
    0xd518_4100, // msr SP_EL0, x0
    0xd284_0000, // mov x0, #0x2000
    0x9100_001f, // mov sp, x0
    0xd286_0000, // mov x0, #0x3000
    0xd518_4020, // msr ELR_EL1, x0
    0xd280_78a0, // mov x0, #0x3c5
    0xd518_4000, // msr SPSR_EL1, x0
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_AFSR0_EL1: u64 = 0x1111;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_AFSR1_EL1: u64 = 0x2222;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_ESR_EL1: u64 = 0x9600_0045;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_FAR_EL1: u64 = 0x3333_4444;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_PAR_EL1: u64 = 0x5555_6800;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_TEST_VBAR_EL1: u64 = 0x1234_5000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXCEPTION_REGISTER_GUEST_CODE: [u32; 18] = [
    0xd282_2220, // mov x0, #0x1111
    0xd518_5100, // msr AFSR0_EL1, x0
    0xd284_4440, // mov x0, #0x2222
    0xd518_5120, // msr AFSR1_EL1, x0
    0xd280_08a0, // mov x0, #0x45
    0xf2b2_c000, // movk x0, #0x9600, lsl #16
    0xd518_5200, // msr ESR_EL1, x0
    0xd288_8880, // mov x0, #0x4444
    0xf2a6_6660, // movk x0, #0x3333, lsl #16
    0xd518_6000, // msr FAR_EL1, x0
    0xd28d_0000, // mov x0, #0x6800
    0xf2aa_aaa0, // movk x0, #0x5555, lsl #16
    0xd518_7400, // msr PAR_EL1, x0
    0xd28a_0000, // mov x0, #0x5000
    0xf2a2_4680, // movk x0, #0x1234, lsl #16
    0xd518_c000, // msr VBAR_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXECUTION_CONTROL_TEST_ACTLR_EL1: u64 = 2;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXECUTION_CONTROL_TEST_CPACR_EL1: u64 = 0x0030_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const EXECUTION_CONTROL_GUEST_CODE: [u32; 6] = [
    0xd280_0040, // mov x0, #2
    0xd518_1020, // msr ACTLR_EL1, x0
    0xd2a0_0600, // mov x0, #0x300000
    0xd518_1040, // msr CPACR_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_TTBR0_EL1: u64 = 0x1234_5000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_TTBR1_EL1: u64 = 0x5678_9000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_TCR_EL1: u64 = 0x10;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_MAIR_EL1: u64 = 0xff44_0400;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_AMAIR_EL1_WRITE: u64 = 0x1122_3344_5566_7788;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_TEST_CONTEXTIDR_EL1: u64 = 0xa5a5_5a5a;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRANSLATION_REGISTER_GUEST_CODE: [u32; 24] = [
    0xd538_1000, // mrs x0, SCTLR_EL1
    0xd518_1000, // msr SCTLR_EL1, x0
    0xd503_3fdf, // isb
    0xd28a_0000, // mov x0, #0x5000
    0xf2a2_4680, // movk x0, #0x1234, lsl #16
    0xd518_2000, // msr TTBR0_EL1, x0
    0xd292_0000, // mov x0, #0x9000
    0xf2aa_cf00, // movk x0, #0x5678, lsl #16
    0xd518_2020, // msr TTBR1_EL1, x0
    0xd280_0200, // mov x0, #0x10
    0xd518_2040, // msr TCR_EL1, x0
    0xd280_8000, // mov x0, #0x400
    0xf2bf_e880, // movk x0, #0xff44, lsl #16
    0xd518_a200, // msr MAIR_EL1, x0
    0xd28e_f100, // mov x0, #0x7788
    0xf2aa_acc0, // movk x0, #0x5566, lsl #16
    0xf2c6_6880, // movk x0, #0x3344, lsl #32
    0xf2e2_2440, // movk x0, #0x1122, lsl #48
    0xd518_a300, // msr AMAIR_EL1, x0
    0xd28b_4b40, // mov x0, #0x5a5a
    0xf2b4_b4a0, // movk x0, #0xa5a5, lsl #16
    0xd518_d020, // msr CONTEXTIDR_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APIA_KEY: u128 = (0x2222_u128 << 64) | 0x1111;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APIB_KEY: u128 = (0x4444_u128 << 64) | 0x3333;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APDA_KEY: u128 = (0x6666_u128 << 64) | 0x5555;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APDB_KEY: u128 = (0x8888_u128 << 64) | 0x7777;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_TEST_APGA_KEY: u128 = (0xaaaa_u128 << 64) | 0x9999;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const POINTER_AUTHENTICATION_KEY_GUEST_CODE: [u32; 22] = [
    0xd282_2220, // mov x0, #0x1111
    0xd518_2100, // msr APIAKeyLo_EL1, x0
    0xd284_4440, // mov x0, #0x2222
    0xd518_2120, // msr APIAKeyHi_EL1, x0
    0xd286_6660, // mov x0, #0x3333
    0xd518_2140, // msr APIBKeyLo_EL1, x0
    0xd288_8880, // mov x0, #0x4444
    0xd518_2160, // msr APIBKeyHi_EL1, x0
    0xd28a_aaa0, // mov x0, #0x5555
    0xd518_2200, // msr APDAKeyLo_EL1, x0
    0xd28c_ccc0, // mov x0, #0x6666
    0xd518_2220, // msr APDAKeyHi_EL1, x0
    0xd28e_eee0, // mov x0, #0x7777
    0xd518_2240, // msr APDBKeyLo_EL1, x0
    0xd291_1100, // mov x0, #0x8888
    0xd518_2260, // msr APDBKeyHi_EL1, x0
    0xd293_3320, // mov x0, #0x9999
    0xd518_2300, // msr APGAKeyLo_EL1, x0
    0xd295_5540, // mov x0, #0xaaaa
    0xd518_2320, // msr APGAKeyHi_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_TEST_TPIDR_EL0: u64 = 0x1111;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_TEST_TPIDRRO_EL0: u64 = 0x2222;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_TEST_TPIDR_EL1: u64 = 0x3333;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const THREAD_CONTEXT_REGISTER_GUEST_CODE: [u32; 7] = [
    0xd282_2220, // mov x0, #0x1111
    0xd51b_d040, // msr TPIDR_EL0, x0
    0xd284_4440, // mov x0, #0x2222
    0xd51b_d060, // msr TPIDRRO_EL0, x0
    0xd286_6660, // mov x0, #0x3333
    0xd518_d080, // msr TPIDR_EL1, x0
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_Q0: [u8; 16] = [0x12; 16];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_Q31: [u8; 16] = [0x34; 16];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_FPCR: u64 = 0x0100_0000;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_TEST_FPSR: u64 = 0x1f;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const SIMD_FP_REGISTER_GUEST_CODE: [u32; 10] = [
    0xd2a0_0600, // mov x0, #0x300000
    0xd518_1040, // msr CPACR_EL1, x0
    0xd503_3fdf, // isb
    0x4f00_e640, // movi v0.16b, #0x12
    0x4f01_e69f, // movi v31.16b, #0x34
    0xd2a0_2000, // mov x0, #0x1000000
    0xd51b_4400, // msr FPCR, x0
    0xd280_03e0, // mov x0, #0x1f
    0xd51b_4420, // msr FPSR, x0
    0xd400_0002, // hvc #0
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_TEST_PMR_EL1: u64 = 0xa0;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_TEST_BPR0_EL1: u64 = 3;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_TEST_BPR1_EL1: u64 = 4;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_ICC_REGISTER_GUEST_CODE: [u32; 15] = [
    0xd538_cca0, // mrs x0, ICC_SRE_EL1
    0xb240_0000, // orr x0, x0, #1
    0xd518_cca0, // msr ICC_SRE_EL1, x0
    0xd503_3fdf, // isb
    0xd280_1400, // mov x0, #0xa0
    0xd518_4600, // msr ICC_PMR_EL1, x0
    0xd280_0060, // mov x0, #3
    0xd518_c860, // msr ICC_BPR0_EL1, x0
    0xd280_0080, // mov x0, #4
    0xd518_cc60, // msr ICC_BPR1_EL1, x0
    0xd280_0020, // mov x0, #1
    0xd518_ccc0, // msr ICC_IGRPEN0_EL1, x0
    0xd518_cce0, // msr ICC_IGRPEN1_EL1, x0
    0xd503_3fdf, // isb
    0xd400_0002, // hvc #0
];

// Bare EL1 setup for one message-only SPI. X0 points at four little-endian
// values: distributor base, redistributor base, INTID, and VBAR. The code
// wakes redistributor 0, programs the SPI as Group-1 edge-triggered and routed
// to affinity 0, enables the GICv3 system-register interface, then publishes
// readiness with HVC #0 and waits for the message.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_MSI_GUEST_CODE: [u32; 69] = [
    0xaa00_03f3, // mov x19, x0
    0xf940_0274, // ldr x20, [x19]                 (GICD)
    0xf940_0675, // ldr x21, [x19, #8]             (GICR)
    0xb940_1276, // ldr w22, [x19, #16]            (INTID)
    0xf940_0e77, // ldr x23, [x19, #24]            (VBAR)
    0xd518_c017, // msr VBAR_EL1, x23
    0xd503_3fdf, // isb
    0x9100_52a1, // add x1, x21, #0x14             (GICR_WAKER)
    0xb940_0022, // ldr w2, [x1]
    0x121e_7842, // bic w2, w2, #2                 (ProcessorSleep)
    0xb900_0022, // str w2, [x1]
    0xb940_0022, // ldr w2, [x1]
    0x3717_ffe2, // tbnz w2, #2, .-4               (ChildrenAsleep)
    0x1200_12c3, // and w3, w22, #31
    0x5280_0024, // mov w4, #1
    0x1ac3_2084, // lsl w4, w4, w3
    0x5305_7ec5, // lsr w5, w22, #5
    0x9102_0286, // add x6, x20, #0x80             (GICD_IGROUPR)
    0x8b05_08c6, // add x6, x6, x5, lsl #2
    0xb940_00c7, // ldr w7, [x6]
    0x2a04_00e7, // orr w7, w7, w4
    0xb900_00c7, // str w7, [x6]
    0x1200_0ec3, // and w3, w22, #15
    0x531f_7863, // lsl w3, w3, #1
    0x1100_0463, // add w3, w3, #1
    0x5280_0024, // mov w4, #1
    0x1ac3_2084, // lsl w4, w4, w3
    0x9130_0286, // add x6, x20, #0xc00            (GICD_ICFGR)
    0x5304_7ec5, // lsr w5, w22, #4
    0x8b05_08c6, // add x6, x6, x5, lsl #2
    0xb940_00c7, // ldr w7, [x6]
    0x2a04_00e7, // orr w7, w7, w4
    0xb900_00c7, // str w7, [x6]
    0x9110_0286, // add x6, x20, #0x400            (GICD_IPRIORITYR)
    0x8b16_00c6, // add x6, x6, x22
    0x5280_1007, // mov w7, #0x80
    0x3900_00c7, // strb w7, [x6]
    0x9140_1a86, // add x6, x20, #0x6000           (GICD_IROUTER)
    0x8b16_0cc6, // add x6, x6, x22, lsl #3
    0xf900_00df, // str xzr, [x6]
    0x1200_12c3, // and w3, w22, #31
    0x5280_0024, // mov w4, #1
    0x1ac3_2084, // lsl w4, w4, w3
    0x5305_7ec5, // lsr w5, w22, #5
    0x9104_0286, // add x6, x20, #0x100            (GICD_ISENABLER)
    0x8b05_08c6, // add x6, x6, x5, lsl #2
    0xb900_00c4, // str w4, [x6]
    0xb940_0287, // ldr w7, [x20]                  (GICD_CTLR)
    0x5280_0248, // mov w8, #0x12                  (ARE_NS | EnableGrp1NS)
    0x2a08_00e7, // orr w7, w7, w8
    0xb900_0287, // str w7, [x20]
    0xd503_3f9f, // dsb sy
    0xb940_0287, // ldr w7, [x20]
    0x37ff_ffe7, // tbnz w7, #31, .-4              (RWP)
    0xd538_cca1, // mrs x1, ICC_SRE_EL1
    0xb240_0021, // orr x1, x1, #1
    0xd518_cca1, // msr ICC_SRE_EL1, x1
    0xd503_3fdf, // isb
    0xd280_1fe1, // mov x1, #0xff
    0xd518_4601, // msr ICC_PMR_EL1, x1
    0xd518_cc7f, // msr ICC_BPR1_EL1, xzr
    0xd280_0021, // mov x1, #1
    0xd518_cce1, // msr ICC_IGRPEN1_EL1, x1
    0xd503_3fdf, // isb
    0xd503_42ff, // msr DAIFClr, #2
    0xb940_0681, // ldr w1, [x20, #4]              (GICD_TYPER evidence)
    0xd400_0002, // hvc #0                         (ready)
    0xd503_207f, // wfi
    0x17ff_ffff, // b .-4
];

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GIC_MSI_IRQ_HANDLER: [u32; 4] = [
    0xd538_cc00, // mrs x0, ICC_IAR1_EL1
    0xd518_cc20, // msr ICC_EOIR1_EL1, x0
    0xd400_0022, // hvc #1
    0x1400_0000, // b .
];

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn test_rtc_mmio_layout() -> bangbang_runtime::rtc::RtcMmioLayout {
    bangbang_runtime::rtc::RtcMmioLayout::new(
        bangbang_runtime::memory::GuestAddress::new(0x4000_1000),
        bangbang_runtime::mmio::MmioRegionId::new(3000),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn queries_arm64_sme_configuration_before_vm_creation() {
    use bangbang_hvf::HvfBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let first =
        assert_sme_configuration_supported_or_unavailable(HvfBackend::arm64_sme_configuration())
            .expect("first SME configuration query should succeed or report unsupported");
    let second =
        assert_sme_configuration_supported_or_unavailable(HvfBackend::arm64_sme_configuration())
            .expect("second SME configuration query should succeed or report unsupported");

    assert!(
        first.is_some() == second.is_some(),
        "SME configuration availability should remain stable on one host"
    );
    if let (Some(first), Some(second)) = (first, second) {
        let first_max_svl_bytes = first.max_svl_bytes();
        let second_max_svl_bytes = second.max_svl_bytes();
        assert!(
            first_max_svl_bytes == second_max_svl_bytes,
            "maximum guest-usable SME SVL should remain stable on one host"
        );
        assert!(
            first == second,
            "SME configuration should remain stable on one host"
        );
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn queries_arm64_default_vcpu_cache_configuration_before_vm_creation() {
    use bangbang_hvf::{HvfArm64VcpuCacheConfiguration, HvfBackend};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let first = HvfBackend::arm64_vcpu_cache_configuration()
        .expect("first default vCPU cache configuration query should succeed");
    let second = HvfBackend::arm64_vcpu_cache_configuration()
        .expect("second default vCPU cache configuration query should succeed");

    let values = |configuration: HvfArm64VcpuCacheConfiguration| {
        [
            configuration.ctr_el0(),
            configuration.clidr_el1(),
            configuration.dczid_el0(),
        ]
    };
    assert!(
        values(first) == values(second),
        "default vCPU cache feature accessors should remain stable on one host"
    );
    assert!(
        first == second,
        "default vCPU cache configuration should remain stable on one host"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn queries_arm64_default_vcpu_cache_geometry_before_vm_creation() {
    use bangbang_hvf::HvfBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let first = HvfBackend::arm64_vcpu_cache_geometry()
        .expect("first default vCPU cache geometry query should succeed");
    let second = HvfBackend::arm64_vcpu_cache_geometry()
        .expect("second default vCPU cache geometry query should succeed");

    assert!(
        first.data_or_unified_ccsidr_el1() == second.data_or_unified_ccsidr_el1()
            && first.instruction_ccsidr_el1() == second.instruction_ccsidr_el1(),
        "default vCPU CCSIDR accessors should remain stable on one host"
    );
    assert!(
        first == second,
        "default vCPU cache geometry should remain stable on one host"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn creates_and_destroys_hvf_vcpu() {
    use bangbang_hvf::{HvfBackend, HvfRegister, HvfSystemRegister};
    use bangbang_runtime::BackendError;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let mut vcpu = backend.create_vcpu().expect("vCPU should be created");
        assert_eq!(
            vcpu.exit_snapshot(),
            Err(BackendError::InvalidState("vCPU has not exited yet"))
        );
        vcpu.set_register(HvfRegister::X0, 0x1234)
            .expect("vCPU register should be set");
        assert_eq!(
            vcpu.get_register(HvfRegister::X0)
                .expect("vCPU register should be read"),
            0x1234
        );
        let original_vtimer_mask = vcpu
            .get_vtimer_mask()
            .expect("original vCPU vtimer mask should be read");
        let original_vtimer_offset = vcpu
            .get_vtimer_offset()
            .expect("original vCPU vtimer offset should be read");
        let original_vtimer_control = vcpu
            .get_system_register(HvfSystemRegister::CNTV_CTL_EL0)
            .expect("original vCPU vtimer control should be read");
        let original_vtimer_compare_value = vcpu
            .get_system_register(HvfSystemRegister::CNTV_CVAL_EL0)
            .expect("original vCPU vtimer compare value should be read");
        vcpu.set_vtimer_mask(true)
            .expect("vCPU vtimer mask should be set");
        vcpu.set_system_register(HvfSystemRegister::CNTV_CTL_EL0, 0)
            .expect("vCPU vtimer should be disabled");
        vcpu.set_vtimer_offset(VTIMER_TEST_OFFSET)
            .expect("vCPU vtimer offset should be set");
        vcpu.set_system_register(HvfSystemRegister::CNTV_CVAL_EL0, VTIMER_TEST_COMPARE_VALUE)
            .expect("vCPU vtimer compare value should be set");
        assert!(
            vcpu.get_vtimer_mask()
                .expect("vCPU vtimer mask should be read")
        );
        assert_eq!(
            vcpu.get_vtimer_offset()
                .expect("vCPU vtimer offset should be read"),
            VTIMER_TEST_OFFSET
        );
        assert_eq!(
            vcpu.get_system_register(HvfSystemRegister::CNTV_CTL_EL0)
                .expect("vCPU vtimer control should be read")
                & VTIMER_WRITABLE_CONTROL_MASK,
            0
        );
        assert_eq!(
            vcpu.get_system_register(HvfSystemRegister::CNTV_CVAL_EL0)
                .expect("vCPU vtimer compare value should be read"),
            VTIMER_TEST_COMPARE_VALUE
        );
        vcpu.set_vtimer_offset(original_vtimer_offset)
            .expect("original vCPU vtimer offset should be restored");
        vcpu.set_system_register(
            HvfSystemRegister::CNTV_CVAL_EL0,
            original_vtimer_compare_value,
        )
        .expect("original vCPU vtimer compare value should be restored");
        vcpu.set_system_register(
            HvfSystemRegister::CNTV_CTL_EL0,
            original_vtimer_control & VTIMER_WRITABLE_CONTROL_MASK,
        )
        .expect("original vCPU vtimer control should be restored");
        vcpu.set_vtimer_mask(original_vtimer_mask)
            .expect("original vCPU vtimer mask should be restored");
        vcpu.destroy().expect("vCPU should be destroyed");
        vcpu.destroy()
            .expect("destroyed vCPU should remain destroyed");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn configures_hvf_vcpu_arm64_boot_registers() {
    use bangbang_hvf::{
        ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfBackend, HvfRegister, HvfSystemRegister,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::GuestAddress;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let registers = HvfArm64BootRegisters {
        kernel_entry: GuestAddress::new(0x8028_0000),
        fdt_address: GuestAddress::new(0x8fe0_0000),
    };

    backend.create_vm().expect("VM should be created");
    {
        let mut vcpu = backend.create_vcpu().expect("vCPU should be created");
        vcpu.configure_arm64_boot_registers(registers)
            .expect("boot registers should be configured");

        assert_eq!(
            vcpu.get_register(HvfRegister::PC)
                .expect("PC should be read"),
            registers.kernel_entry.raw_value()
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X0)
                .expect("X0 should be read"),
            registers.fdt_address.raw_value()
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X1)
                .expect("X1 should be read"),
            0
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X2)
                .expect("X2 should be read"),
            0
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::X3)
                .expect("X3 should be read"),
            0
        );
        assert_eq!(
            vcpu.get_register(HvfRegister::CPSR)
                .expect("CPSR should be read"),
            ARM64_LINUX_BOOT_CPSR
        );
        let _mpidr = vcpu
            .get_system_register(HvfSystemRegister::MPIDR_EL1)
            .expect("MPIDR_EL1 should be read");

        vcpu.destroy().expect("vCPU should be destroyed");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_configured_arm64_general_registers_on_runner_thread() {
    use bangbang_hvf::{ARM64_LINUX_BOOT_CPSR, HvfArm64BootRegisters, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::GuestAddress;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let registers = HvfArm64BootRegisters {
        kernel_entry: GuestAddress::new(0x8028_0000),
        fdt_address: GuestAddress::new(0x8fe0_0000),
    };

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(registers)
            .expect("boot registers should be configured");

        let state = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be captured");
        assert_eq!(state.general_purpose_registers().len(), 31);
        assert_eq!(
            state.general_purpose_register(0),
            Some(registers.fdt_address.raw_value())
        );
        assert_eq!(state.general_purpose_register(1), Some(0));
        assert_eq!(state.general_purpose_register(2), Some(0));
        assert_eq!(state.general_purpose_register(3), Some(0));
        assert_eq!(state.pc(), registers.kernel_entry.raw_value());
        assert_eq!(state.cpsr(), ARM64_LINUX_BOOT_CPSR);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn measures_real_hvf_vcpu_execution_time_on_owner_thread() {
    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit,
        is_hvf_arm64_pvtime_measurement_available,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    const NOP: u32 = 0xd503_201f;
    const HVC_ZERO: u32 = 0xd400_0002;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    assert!(
        is_hvf_arm64_pvtime_measurement_available(),
        "the signed host must export the public macOS 11 execution-time primitive"
    );
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = std::iter::repeat_n(NOP, 128)
        .chain([HVC_ZERO])
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("execution-time guest should fit");

    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend
        .create_gic()
        .expect("GIC should be created before the execution-time vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest boot registers should configure");

        let before = runner
            .pvtime_execution_time_ns()
            .expect("initial owner-thread measurement should succeed");
        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("measurement guest should exit through HVC")
        else {
            panic!("measurement guest should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("measurement guest exit should decode")
                .immediate(),
            0
        );
        let after = runner
            .pvtime_execution_time_ns()
            .expect("post-run owner-thread measurement should succeed");
        let repeated = runner
            .pvtime_execution_time_ns()
            .expect("repeated owner-thread measurement should succeed");
        assert!(
            after > before,
            "guest execution must increase cumulative time"
        );
        assert!(
            repeated >= after,
            "cumulative execution time must not regress"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_arm64_general_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::GuestAddress;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let registers = HvfArm64BootRegisters {
        kernel_entry: GuestAddress::new(0x8028_0000),
        fdt_address: GuestAddress::new(0x8fe0_0000),
    };

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(registers)
            .expect("boot registers should be configured");

        let before = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be captured before restore");
        runner
            .restore_arm64_general_register_state(&before)
            .expect("general-register state should be restored");
        let after = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be recaptured after restore");
        assert!(
            after == before,
            "general-register state should round trip without exposing values"
        );

        runner
            .restore_arm64_general_register_state(&before)
            .expect("repeated general-register restore should succeed");
        let repeated = runner
            .capture_arm64_general_register_state()
            .expect("general-register state should be recaptured after repeated restore");
        assert!(
            repeated == before,
            "repeated general-register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_core_system_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = CORE_SYSTEM_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("core system-register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest register writer should exit through HVC")
        else {
            panic!("guest register writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest register writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_core_system_register_state()
            .expect("core system-register state should be captured");
        assert_eq!(state.sp_el0(), CORE_SYSTEM_TEST_SP_EL0);
        assert_eq!(state.sp_el1(), CORE_SYSTEM_TEST_SP_EL1);
        assert_eq!(state.elr_el1(), CORE_SYSTEM_TEST_ELR_EL1);
        assert_eq!(state.spsr_el1(), CORE_SYSTEM_TEST_SPSR_EL1);

        runner
            .restore_arm64_core_system_register_state(&state)
            .expect("core system-register state should be restored");
        let restored = runner
            .capture_arm64_core_system_register_state()
            .expect("core system-register state should be recaptured after restore");
        assert!(
            restored == state,
            "core system-register state should round trip without exposing values"
        );

        runner
            .restore_arm64_core_system_register_state(&state)
            .expect("repeated core system-register restore should succeed");
        let repeated = runner
            .capture_arm64_core_system_register_state()
            .expect("core system-register state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated core system-register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_exception_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = EXCEPTION_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("exception-register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest exception-register writer should exit through HVC")
        else {
            panic!("guest exception-register writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest exception-register writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_exception_register_state()
            .expect("exception-register state should be captured");
        // Auxiliary fault-status contents are implementation-defined. Current
        // Apple Silicon exposes AFSR0 as read-as-zero/write-ignored and
        // preserves AFSR1, while another host may expose either behavior for
        // either register.
        assert!(matches!(state.afsr0_el1(), 0 | EXCEPTION_TEST_AFSR0_EL1));
        assert!(matches!(state.afsr1_el1(), 0 | EXCEPTION_TEST_AFSR1_EL1));
        assert_eq!(state.esr_el1(), EXCEPTION_TEST_ESR_EL1);
        assert_eq!(state.far_el1(), EXCEPTION_TEST_FAR_EL1);
        assert_eq!(state.par_el1(), EXCEPTION_TEST_PAR_EL1);
        assert_eq!(state.vbar_el1(), EXCEPTION_TEST_VBAR_EL1);

        runner
            .restore_arm64_exception_register_state(&state)
            .expect("exception-register state should be restored");
        let restored = runner
            .capture_arm64_exception_register_state()
            .expect("exception-register state should be recaptured after restore");
        assert!(
            restored == state,
            "exception-register state should round trip without exposing values"
        );

        runner
            .restore_arm64_exception_register_state(&state)
            .expect("repeated exception-register restore should succeed");
        let repeated = runner
            .capture_arm64_exception_register_state()
            .expect("exception-register state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated exception-register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_execution_controls_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = EXECUTION_CONTROL_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("execution-control guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest execution-control writer should exit through HVC")
        else {
            panic!("guest execution-control writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest execution-control writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_execution_control_register_state()
            .expect("execution-control state should be captured");
        assert_eq!(state.actlr_el1(), EXECUTION_CONTROL_TEST_ACTLR_EL1);
        assert_eq!(state.cpacr_el1(), EXECUTION_CONTROL_TEST_CPACR_EL1);

        runner
            .restore_arm64_execution_control_register_state(&state)
            .expect("execution-control state should be restored");
        let restored = runner
            .capture_arm64_execution_control_register_state()
            .expect("execution-control state should be recaptured after restore");
        assert!(
            restored == state,
            "execution-control state should round trip without exposing values"
        );

        runner
            .restore_arm64_execution_control_register_state(&state)
            .expect("repeated execution-control restore should succeed");
        let repeated = runner
            .capture_arm64_execution_control_register_state()
            .expect("execution-control state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated execution-control restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_arm64_cache_selection_register_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_cache_selection_register_state()
            .expect("first cache-selection state should be captured");
        let second = runner
            .capture_arm64_cache_selection_register_state()
            .expect("second cache-selection state should be captured");

        // Exercise the raw accessor without assuming an architecturally
        // unknown reset value or interpreting it as cache topology.
        let _captured_values = [first.csselr_el1(), second.csselr_el1()];

        runner
            .restore_arm64_cache_selection_register_state(&first)
            .expect("cache-selection state should be restored");
        let restored = runner
            .capture_arm64_cache_selection_register_state()
            .expect("restored cache-selection state should be captured");
        assert!(
            restored == first,
            "restored cache-selection state should match its idle source"
        );
        runner
            .restore_arm64_cache_selection_register_state(&first)
            .expect("cache-selection state should be restored a second time");
        let restored_again = runner
            .capture_arm64_cache_selection_register_state()
            .expect("twice-restored cache-selection state should be captured");
        assert!(
            restored_again == first,
            "twice-restored cache-selection state should match its idle source"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_all_implemented_arm64_breakpoint_registers_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_breakpoint_register_state()
            .expect("first breakpoint-register state should be captured");
        let second = runner
            .capture_arm64_breakpoint_register_state()
            .expect("second breakpoint-register state should be captured");

        for state in [&first, &second] {
            let count = state.implemented_breakpoint_count();
            assert!((1..=16).contains(&count));
            assert_eq!(state.breakpoint_value_registers().len(), usize::from(count));
            assert_eq!(
                state.breakpoint_control_registers().len(),
                usize::from(count)
            );
            for index in 0..count {
                assert!(state.breakpoint_value_register(index).is_some());
                assert!(state.breakpoint_control_register(index).is_some());
            }
            if count < 16 {
                assert_eq!(state.breakpoint_value_register(count), None);
                assert_eq!(state.breakpoint_control_register(count), None);
            }
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_all_implemented_arm64_watchpoint_registers_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_watchpoint_register_state()
            .expect("first watchpoint-register state should be captured");
        let second = runner
            .capture_arm64_watchpoint_register_state()
            .expect("second watchpoint-register state should be captured");

        for state in [&first, &second] {
            let count = state.implemented_watchpoint_count();
            assert!((1..=16).contains(&count));
            assert_eq!(state.watchpoint_value_registers().len(), usize::from(count));
            assert_eq!(
                state.watchpoint_control_registers().len(),
                usize::from(count)
            );
            for index in 0..count {
                assert!(state.watchpoint_value_register(index).is_some());
                assert!(state.watchpoint_control_register(index).is_some());
            }
            if count < 16 {
                assert_eq!(state.watchpoint_value_register(count), None);
                assert_eq!(state.watchpoint_control_register(count), None);
            }
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_arm64_debug_control_registers_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let original = runner
            .capture_arm64_debug_control_register_state()
            .expect("original debug-control state should be captured");
        runner
            .restore_arm64_debug_control_register_state(&original)
            .expect("first debug-control state restore should succeed");
        let first_recapture = runner
            .capture_arm64_debug_control_register_state()
            .expect("debug-control state should be recaptured after first restore");
        assert_eq!(first_recapture, original);
        runner
            .restore_arm64_debug_control_register_state(&original)
            .expect("second debug-control state restore should succeed");
        let second_recapture = runner
            .capture_arm64_debug_control_register_state()
            .expect("debug-control state should be recaptured after second restore");
        assert_eq!(second_recapture, original);

        // Exercise both accessors without assuming or logging reset values.
        // Reapplying only the captured original does not manufacture active
        // debug controls, touch adjacent debug state, or execute the guest.
        let _captured_values = [
            first_recapture.mdccint_el1(),
            first_recapture.mdscr_el1(),
            second_recapture.mdccint_el1(),
            second_recapture.mdscr_el1(),
        ];

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_arm64_debug_trap_state_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let original = runner
            .capture_arm64_debug_trap_state()
            .expect("original debug-trap state should be captured");
        runner
            .restore_arm64_debug_trap_state(&original)
            .expect("first debug-trap state restore should succeed");
        let first_recapture = runner
            .capture_arm64_debug_trap_state()
            .expect("debug-trap state should be recaptured after first restore");
        assert_eq!(first_recapture, original);
        runner
            .restore_arm64_debug_trap_state(&original)
            .expect("second debug-trap state restore should succeed");
        let second_recapture = runner
            .capture_arm64_debug_trap_state()
            .expect("debug-trap state should be recaptured after second restore");
        assert_eq!(second_recapture, original);

        // Exercise both accessors without assuming or logging default values.
        // Reapplying only the captured original keeps this test free of guest
        // debug activation, guest instructions, and destination-policy claims.
        let _captured_values = [
            first_recapture.trap_debug_exceptions(),
            first_recapture.trap_debug_reg_accesses(),
            second_recapture.trap_debug_exceptions(),
            second_recapture.trap_debug_reg_accesses(),
        ];

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_identification_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuIdentificationRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_identification_register_state()
            .expect("first identification-register state should be captured");
        let second = runner
            .capture_arm64_identification_register_state()
            .expect("second identification-register state should be captured");

        let values = |state: HvfArm64VcpuIdentificationRegisterState| {
            [
                state.midr_el1(),
                state.mpidr_el1(),
                state.id_aa64pfr0_el1(),
                state.id_aa64pfr1_el1(),
                state.id_aa64dfr0_el1(),
                state.id_aa64dfr1_el1(),
                state.id_aa64isar0_el1(),
                state.id_aa64isar1_el1(),
                state.id_aa64mmfr0_el1(),
                state.id_aa64mmfr1_el1(),
                state.id_aa64mmfr2_el1(),
            ]
        };
        assert!(
            values(first) == values(second),
            "identification-register accessors should remain stable within one vCPU lifetime"
        );
        assert!(
            first == second,
            "identification-register state should remain stable within one vCPU lifetime"
        );
        assert!(
            first.mpidr_el1()
                == runner
                    .mpidr_el1()
                    .expect("standalone MPIDR owner-thread read should succeed"),
            "captured MPIDR should match the standalone owner-thread getter"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sve_sme_identification_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSveSmeIdentificationRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_sve_sme_identification_register_state()
            .expect("first SVE/SME identification state should be captured");
        let second = runner
            .capture_arm64_sve_sme_identification_register_state()
            .expect("second SVE/SME identification state should be captured");

        let values = |state: HvfArm64VcpuSveSmeIdentificationRegisterState| {
            [state.id_aa64zfr0_el1(), state.id_aa64smfr0_el1()]
        };
        assert!(
            values(first) == values(second),
            "SVE/SME identification accessors should remain stable within one vCPU lifetime"
        );
        assert!(
            first == second,
            "SVE/SME identification state should remain stable within one vCPU lifetime"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_pstate_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first =
            assert_sme_pstate_capture_supported_or_unavailable(runner.capture_arm64_sme_pstate())
                .expect("first SME PSTATE capture should succeed or report unsupported");
        let second =
            assert_sme_pstate_capture_supported_or_unavailable(runner.capture_arm64_sme_pstate())
                .expect("second SME PSTATE capture should succeed or report unsupported");

        assert_eq!(
            first.is_some(),
            second.is_some(),
            "SME availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            // Exercise both accessors without assuming or logging the flags,
            // entering streaming mode, enabling ZA, or reading SME data.
            let first_values = (
                first.streaming_sve_mode_enabled(),
                first.za_storage_enabled(),
            );
            let second_values = (
                second.streaming_sve_mode_enabled(),
                second.za_storage_enabled(),
            );
            assert!(
                first_values == second_values,
                "SME PSTATE should remain stable on one idle vCPU"
            );
            assert!(
                first == second,
                "SME PSTATE value should remain stable on one idle vCPU"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_p_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmePRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_p_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_p_register_state(),
        )
        .expect("first SME P-register capture should succeed or report unavailable");
        let second = assert_sme_p_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_p_register_state(),
        )
        .expect("second SME P-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME P-register capture availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.maximum_svl_bytes() == second.maximum_svl_bytes(),
                "SME maximum streaming vector length should remain stable"
            );
            assert!(
                first.predicate_width_bytes() == second.predicate_width_bytes(),
                "SME predicate allocation width should remain stable"
            );
            assert!(
                first.p_register(15).is_some() && first.p_register(16).is_none(),
                "SME P-register capture should contain exactly P0 through P15"
            );
            for register in 0..HvfArm64VcpuSmePRegisterState::REGISTER_COUNT {
                let first_register = first
                    .p_register(register)
                    .expect("first capture should contain every P register");
                let second_register = second
                    .p_register(register)
                    .expect("second capture should contain every P register");
                assert!(
                    first_register.len() == first.predicate_width_bytes(),
                    "first capture should retain the exact predicate width"
                );
                assert!(
                    second_register.len() == second.predicate_width_bytes(),
                    "second capture should retain the exact predicate width"
                );
            }
            assert!(
                first == second,
                "SME P-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}").contains("<redacted>"),
                "SME P-register debug output should remain redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_z_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmeZRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_z_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_z_register_state(),
        )
        .expect("first SME Z-register capture should succeed or report unavailable");
        let second = assert_sme_z_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_z_register_state(),
        )
        .expect("second SME Z-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME Z-register capture availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.maximum_svl_bytes() == second.maximum_svl_bytes(),
                "SME maximum streaming vector length should remain stable"
            );
            assert!(
                first.z_register(31).is_some() && first.z_register(32).is_none(),
                "SME Z-register capture should contain exactly Z0 through Z31"
            );
            for register in 0..HvfArm64VcpuSmeZRegisterState::REGISTER_COUNT {
                let first_register = first
                    .z_register(register)
                    .expect("first capture should contain every Z register");
                let second_register = second
                    .z_register(register)
                    .expect("second capture should contain every Z register");
                assert!(
                    first_register.len() == first.maximum_svl_bytes(),
                    "first capture should retain the exact maximum width"
                );
                assert!(
                    second_register.len() == second.maximum_svl_bytes(),
                    "second capture should retain the exact maximum width"
                );
            }
            assert!(
                first == second,
                "SME Z-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}").contains("<redacted>"),
                "SME Z-register debug output should remain redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_za_register_on_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_za_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_za_register_state(),
        )
        .expect("first SME ZA-register capture should succeed or report unavailable");
        let second = assert_sme_za_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_za_register_state(),
        )
        .expect("second SME ZA-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME ZA-register capture availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.maximum_svl_bytes() == second.maximum_svl_bytes(),
                "SME maximum streaming vector length should remain stable"
            );
            let expected_size = first
                .maximum_svl_bytes()
                .checked_mul(first.maximum_svl_bytes())
                .expect("SME maximum streaming vector length should have a square byte size");
            assert!(
                first.len() == expected_size && first.as_bytes().len() == expected_size,
                "first SME ZA capture should retain the exact maximum-SVL square"
            );
            assert!(
                second.len() == expected_size && second.as_bytes().len() == expected_size,
                "second SME ZA capture should retain the exact maximum-SVL square"
            );
            assert!(
                !first.is_empty() && !second.is_empty(),
                "successful SME ZA captures should contain the complete matrix"
            );
            assert!(
                first == second,
                "SME ZA-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}").contains("<redacted>"),
                "SME ZA-register debug output should remain redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_zt0_register_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmeZt0RegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = assert_sme_zt0_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_zt0_register_state(),
        )
        .expect("first SME ZT0-register capture should succeed or report unavailable");
        let second = assert_sme_zt0_register_capture_supported_or_unavailable(
            runner.capture_arm64_sme_zt0_register_state(),
        )
        .expect("second SME ZT0-register capture should succeed or report unavailable");

        assert!(
            first.is_some() == second.is_some(),
            "SME ZT0-register availability should remain stable within one vCPU lifetime"
        );
        if let (Some(first), Some(second)) = (first, second) {
            assert!(
                first.as_bytes().len() == HvfArm64VcpuSmeZt0RegisterState::BYTE_COUNT
                    && second.as_bytes().len() == HvfArm64VcpuSmeZt0RegisterState::BYTE_COUNT,
                "SME ZT0 captures should preserve exactly 64 bytes"
            );
            assert!(
                first == second,
                "SME ZT0-register state should remain stable on one idle vCPU"
            );
            assert!(
                format!("{first:?}")
                    == "HvfArm64VcpuSmeZt0RegisterState { register: \"<redacted>\" }",
                "SME ZT0-register debug output should remain fully redacted"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_sme_system_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSmeSystemRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_sme_system_register_state()
            .expect("first SME system-register state should be captured");
        let second = runner
            .capture_arm64_sme_system_register_state()
            .expect("second SME system-register state should be captured");

        let values = |state: HvfArm64VcpuSmeSystemRegisterState| {
            [state.smcr_el1(), state.smpri_el1(), state.tpidr2_el0()]
        };
        assert!(
            values(first) == values(second),
            "SME system-register accessors should remain stable within one idle vCPU lifetime"
        );
        assert!(
            first == second,
            "SME system-register state should remain stable within one idle vCPU lifetime"
        );
        assert!(
            format!("{first:?}").contains("<redacted>"),
            "SME system-register debug output should remain redacted"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_reviewed_optional_arm64_state_on_runner_thread() {
    use bangbang_hvf::{
        HvfArm64DebugRegisterRestoreState, HvfArm64OptionalStateValue,
        HvfArm64ReviewedOptionalStateRestore, HvfArm64SmeRestoreState,
        HvfArm64SmeRestoreStateInput, HvfArm64VcpuBreakpointRegisterState,
        HvfArm64VcpuSmePRegisterState, HvfArm64VcpuSmePstate, HvfArm64VcpuSmeSystemRegisterState,
        HvfArm64VcpuSmeZRegisterState, HvfArm64VcpuWatchpointRegisterState, HvfBackend,
        HvfVcpuRunnerError,
    };
    use bangbang_runtime::VmBackend;

    fn breakpoint_restore(
        state: &HvfArm64VcpuBreakpointRegisterState,
    ) -> HvfArm64DebugRegisterRestoreState {
        let mut values = [HvfArm64OptionalStateValue::DestinationDefault; 16];
        let mut controls = [HvfArm64OptionalStateValue::DestinationDefault; 16];
        for (destination, value) in values.iter_mut().zip(state.breakpoint_value_registers()) {
            *destination = HvfArm64OptionalStateValue::Explicit(*value);
        }
        for (destination, control) in controls
            .iter_mut()
            .zip(state.breakpoint_control_registers())
        {
            assert_eq!(control & 1, 0, "fresh breakpoint controls must be disabled");
            *destination = HvfArm64OptionalStateValue::Explicit(*control);
        }
        HvfArm64DebugRegisterRestoreState::try_new(
            state.implemented_breakpoint_count(),
            values,
            controls,
        )
        .expect("captured breakpoint inventory should be valid")
    }

    fn watchpoint_restore(
        state: &HvfArm64VcpuWatchpointRegisterState,
    ) -> HvfArm64DebugRegisterRestoreState {
        let mut values = [HvfArm64OptionalStateValue::DestinationDefault; 16];
        let mut controls = [HvfArm64OptionalStateValue::DestinationDefault; 16];
        for (destination, value) in values.iter_mut().zip(state.watchpoint_value_registers()) {
            *destination = HvfArm64OptionalStateValue::Explicit(*value);
        }
        for (destination, control) in controls
            .iter_mut()
            .zip(state.watchpoint_control_registers())
        {
            assert_eq!(control & 1, 0, "fresh watchpoint controls must be disabled");
            *destination = HvfArm64OptionalStateValue::Explicit(*control);
        }
        HvfArm64DebugRegisterRestoreState::try_new(
            state.implemented_watchpoint_count(),
            values,
            controls,
        )
        .expect("captured watchpoint inventory should be valid")
    }

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let identification = runner
            .capture_arm64_identification_register_state()
            .expect("destination identification should be captured");
        let breakpoints = runner
            .capture_arm64_breakpoint_register_state()
            .expect("fresh breakpoint state should be captured");
        let watchpoints = runner
            .capture_arm64_watchpoint_register_state()
            .expect("fresh watchpoint state should be captured");
        let simd_fp = runner
            .capture_arm64_simd_fp_state()
            .expect("fresh SIMD/FP state should be captured");
        let fresh_pstate =
            assert_sme_pstate_capture_supported_or_unavailable(runner.capture_arm64_sme_pstate())
                .expect("SME PSTATE should be captured or report unavailable");

        let (expected_sme_version, sme, expected_z, expected_sme_system) =
            if let Some(fresh_pstate) = fresh_pstate {
                assert!(
                    !fresh_pstate.streaming_sve_mode_enabled()
                        && !fresh_pstate.za_storage_enabled(),
                    "a new vCPU should expose inactive SME PSTATE"
                );
                let version = u8::try_from((identification.id_aa64pfr1_el1() >> 24) & 0xf)
                    .expect("SME version field should fit in u8");
                assert!(
                    version <= 2,
                    "reviewed restore should reject an unknown SME feature version"
                );
                let configuration = assert_sme_configuration_supported_or_unavailable(
                    HvfBackend::arm64_sme_configuration(),
                )
                .expect("SME configuration query should not fail")
                .expect("available SME PSTATE should have an SME configuration");
                let maximum_svl_bytes = configuration.max_svl_bytes();
                let sme_identification = runner
                    .capture_arm64_sve_sme_identification_register_state()
                    .expect("SVE/SME identification should be captured");
                let sme_system = runner
                    .capture_arm64_sme_system_register_state()
                    .expect("fresh SME system state should be captured");
                let expected_z: Vec<Box<[u8]>> = simd_fp
                    .q_registers()
                    .iter()
                    .map(|q_register| {
                        let mut value = vec![0; maximum_svl_bytes];
                        value
                            .get_mut(..16)
                            .expect("maximum SVL should contain the Q alias")
                            .copy_from_slice(q_register);
                        value.into_boxed_slice()
                    })
                    .collect();
                let z_registers = expected_z
                    .iter()
                    .cloned()
                    .map(HvfArm64OptionalStateValue::Explicit)
                    .collect();
                let input = HvfArm64SmeRestoreStateInput::new(
                    version,
                    sme_identification,
                    maximum_svl_bytes,
                    HvfArm64OptionalStateValue::Explicit(HvfArm64VcpuSmePstate::new(true, true)),
                    [HvfArm64OptionalStateValue::DestinationDefault; 3],
                )
                .with_streaming_registers(
                    z_registers,
                    vec![HvfArm64OptionalStateValue::DestinationDefault; 16],
                )
                .with_za_register(
                    HvfArm64OptionalStateValue::DestinationDefault,
                    (version >= 1).then_some(HvfArm64OptionalStateValue::DestinationDefault),
                );
                let sme = HvfArm64SmeRestoreState::try_new(input, &simd_fp)
                    .expect("fresh-host SME restore state should be valid");
                (Some(version), Some(sme), Some(expected_z), Some(sme_system))
            } else {
                assert_eq!(
                    (identification.id_aa64pfr1_el1() >> 24) & 0xf,
                    0xf,
                    "unavailable SME state should agree with virtual identification"
                );
                (None, None, None, None)
            };

        let request = HvfArm64ReviewedOptionalStateRestore::try_new(
            identification.id_aa64dfr0_el1(),
            expected_sme_version,
            breakpoint_restore(&breakpoints),
            watchpoint_restore(&watchpoints),
            sme,
            simd_fp.clone(),
        )
        .expect("fresh reviewed optional-state request should be valid");
        let second_attempt = request.clone();

        runner
            .restore_arm64_reviewed_optional_state(request)
            .expect("reviewed optional state should restore on its permanent owner");

        assert_eq!(
            runner
                .capture_arm64_breakpoint_register_state()
                .expect("restored breakpoint state should be captured"),
            breakpoints
        );
        assert_eq!(
            runner
                .capture_arm64_watchpoint_register_state()
                .expect("restored watchpoint state should be captured"),
            watchpoints
        );
        assert_eq!(
            runner
                .capture_arm64_simd_fp_state()
                .expect("restored SIMD/FP state should be captured"),
            simd_fp
        );

        if let (Some(version), Some(expected_z), Some(expected_sme_system)) =
            (expected_sme_version, expected_z, expected_sme_system)
        {
            let restored_pstate = runner
                .capture_arm64_sme_pstate()
                .expect("restored SME PSTATE should be captured");
            assert!(
                restored_pstate.streaming_sve_mode_enabled()
                    && restored_pstate.za_storage_enabled(),
                "reviewed restore should publish the requested active SME PSTATE"
            );
            let restored_z = runner
                .capture_arm64_sme_z_register_state()
                .expect("restored streaming Z state should be captured");
            for (index, expected) in expected_z.iter().enumerate() {
                assert_eq!(
                    restored_z.z_register(index),
                    Some(expected.as_ref()),
                    "restored Z register should match its Q-compatible request"
                );
            }
            let restored_p = runner
                .capture_arm64_sme_p_register_state()
                .expect("restored streaming P state should be captured");
            for index in 0..HvfArm64VcpuSmePRegisterState::REGISTER_COUNT {
                assert!(
                    restored_p
                        .p_register(index)
                        .expect("restored P inventory should be complete")
                        .iter()
                        .all(|byte| *byte == 0),
                    "destination-default P registers should retain transition zero"
                );
            }
            runner
                .capture_arm64_sme_za_register_state()
                .expect("restored destination-default ZA should remain readable");
            if version >= 1 {
                runner
                    .capture_arm64_sme_zt0_register_state()
                    .expect("restored destination-default ZT0 should remain readable");
            }
            let restored_system: HvfArm64VcpuSmeSystemRegisterState = runner
                .capture_arm64_sme_system_register_state()
                .expect("restored SME system state should be captured");
            assert_eq!(restored_system, expected_sme_system);
            assert_eq!(
                restored_z.maximum_svl_bytes(),
                expected_z
                    .first()
                    .expect("restored Z inventory should be nonempty")
                    .len()
            );
            assert_eq!(
                restored_p.predicate_width_bytes(),
                restored_z.maximum_svl_bytes() / 8
            );
            assert_eq!(
                HvfArm64VcpuSmeZRegisterState::REGISTER_COUNT,
                expected_z.len()
            );
        }

        assert!(matches!(
            runner.restore_arm64_reviewed_optional_state(second_attempt),
            Err(HvfVcpuRunnerError::InvalidState(
                "vCPU runner reviewed optional state restore was already attempted"
            ))
        ));
        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_arm64_system_context_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64VcpuSystemContextRegisterState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_system_context_register_state()
            .expect("first system-context register state should be captured");
        let second = runner
            .capture_arm64_system_context_register_state()
            .expect("second system-context register state should be captured");

        let values = |state: HvfArm64VcpuSystemContextRegisterState| {
            [state.scxtnum_el0(), state.scxtnum_el1()]
        };
        assert!(
            values(first) == values(second),
            "system-context register accessors should remain stable within one idle vCPU lifetime"
        );
        assert!(
            first == second,
            "system-context register state should remain stable within one idle vCPU lifetime"
        );
        assert!(
            format!("{first:?}").contains("<redacted>"),
            "system-context register debug output should remain redacted"
        );

        runner
            .restore_arm64_system_context_register_state(&first)
            .expect("system-context register state should be restored");
        let restored = runner
            .capture_arm64_system_context_register_state()
            .expect("restored system-context register state should be captured");
        assert!(
            restored == first,
            "restored system-context register state should match its idle source"
        );
        runner
            .restore_arm64_system_context_register_state(&first)
            .expect("system-context register state should be restored a second time");
        let restored_again = runner
            .capture_arm64_system_context_register_state()
            .expect("twice-restored system-context register state should be captured");
        assert!(
            restored_again == first,
            "twice-restored system-context register state should match its idle source"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_translation_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = TRANSLATION_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("translation-register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest translation-register writer should exit through HVC")
        else {
            panic!("guest translation-register writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest translation-register writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_translation_register_state()
            .expect("translation-register state should be captured");
        assert_eq!(state.sctlr_el1() & 1, 0);
        assert_eq!(state.ttbr0_el1(), TRANSLATION_TEST_TTBR0_EL1);
        assert_eq!(state.ttbr1_el1(), TRANSLATION_TEST_TTBR1_EL1);
        assert_eq!(state.tcr_el1(), TRANSLATION_TEST_TCR_EL1);
        assert_eq!(state.mair_el1(), TRANSLATION_TEST_MAIR_EL1);
        // AMAIR is implementation-defined. Current Apple Silicon exposes it
        // as read-as-zero/write-ignored, while a future host may preserve the
        // architecturally valid guest write.
        assert!(matches!(
            state.amair_el1(),
            0 | TRANSLATION_TEST_AMAIR_EL1_WRITE
        ));
        assert_eq!(state.contextidr_el1(), TRANSLATION_TEST_CONTEXTIDR_EL1);

        runner
            .restore_arm64_translation_register_state(&state)
            .expect("translation-register state should be restored");
        let restored = runner
            .capture_arm64_translation_register_state()
            .expect("translation-register state should be recaptured after restore");
        assert!(
            restored == state,
            "translation-register state should round trip without exposing values"
        );

        runner
            .restore_arm64_translation_register_state(&state)
            .expect("repeated translation-register restore should succeed");
        let repeated = runner
            .capture_arm64_translation_register_state()
            .expect("translation-register state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated translation-register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_pointer_authentication_keys_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = POINTER_AUTHENTICATION_KEY_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("pointer-authentication key guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest pointer-authentication key writer should exit through HVC")
        else {
            panic!("guest pointer-authentication key writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest pointer-authentication key writer should exit through HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_pointer_authentication_key_state()
            .expect("pointer-authentication key state should be captured");
        assert!(
            format!("{state:?}")
                == "HvfArm64VcpuPointerAuthenticationKeyState { keys: \"<redacted>\" }",
            "pointer-authentication key Debug output should be fully redacted"
        );
        assert!(
            state.apia_key() == POINTER_AUTHENTICATION_TEST_APIA_KEY,
            "APIA should match the non-secret test key"
        );
        assert!(
            state.apib_key() == POINTER_AUTHENTICATION_TEST_APIB_KEY,
            "APIB should match the non-secret test key"
        );
        assert!(
            state.apda_key() == POINTER_AUTHENTICATION_TEST_APDA_KEY,
            "APDA should match the non-secret test key"
        );
        assert!(
            state.apdb_key() == POINTER_AUTHENTICATION_TEST_APDB_KEY,
            "APDB should match the non-secret test key"
        );
        assert!(
            state.apga_key() == POINTER_AUTHENTICATION_TEST_APGA_KEY,
            "APGA should match the non-secret test key"
        );

        runner
            .restore_arm64_pointer_authentication_key_state(&state)
            .expect("pointer-authentication key state should be restored");
        let restored = runner
            .capture_arm64_pointer_authentication_key_state()
            .expect("pointer-authentication key state should be recaptured after restore");
        assert!(
            restored == state,
            "pointer-authentication key state should round trip without exposing values"
        );

        runner
            .restore_arm64_pointer_authentication_key_state(&state)
            .expect("repeated pointer-authentication key restore should succeed");
        let repeated = runner
            .capture_arm64_pointer_authentication_key_state()
            .expect("pointer-authentication key state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated pointer-authentication key restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_thread_context_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = THREAD_CONTEXT_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("thread-context register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest thread-context writer should exit through HVC")
        else {
            panic!("guest thread-context writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest thread-context writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_thread_context_register_state()
            .expect("thread-context register state should be captured");
        assert_eq!(state.tpidr_el0(), THREAD_CONTEXT_TEST_TPIDR_EL0);
        assert_eq!(state.tpidrro_el0(), THREAD_CONTEXT_TEST_TPIDRRO_EL0);
        assert_eq!(state.tpidr_el1(), THREAD_CONTEXT_TEST_TPIDR_EL1);

        runner
            .restore_arm64_thread_context_register_state(&state)
            .expect("thread-context register state should be restored");
        let restored = runner
            .capture_arm64_thread_context_register_state()
            .expect("thread-context register state should be recaptured after restore");
        assert!(
            restored == state,
            "thread-context register state should round trip without exposing values"
        );

        runner
            .restore_arm64_thread_context_register_state(&state)
            .expect("repeated thread-context register restore should succeed");
        let repeated = runner
            .capture_arm64_thread_context_register_state()
            .expect("thread-context register state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated thread-context register restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_guest_written_arm64_simd_fp_state_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = SIMD_FP_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("SIMD/FP guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest SIMD/FP writer should exit through HVC")
        else {
            panic!("guest SIMD/FP writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest SIMD/FP writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_simd_fp_state()
            .expect("SIMD/FP state should be captured");
        assert_eq!(state.q_register(0), Some(SIMD_FP_TEST_Q0));
        assert_eq!(state.q_register(31), Some(SIMD_FP_TEST_Q31));
        assert_eq!(state.fpcr(), SIMD_FP_TEST_FPCR);
        assert_eq!(state.fpsr(), SIMD_FP_TEST_FPSR);

        runner
            .restore_arm64_simd_fp_state(&state)
            .expect("SIMD/FP state should be restored");
        let restored = runner
            .capture_arm64_simd_fp_state()
            .expect("SIMD/FP state should be recaptured after restore");
        assert!(
            restored == state,
            "SIMD/FP state should round trip without exposing values"
        );

        runner
            .restore_arm64_simd_fp_state(&state)
            .expect("repeated SIMD/FP restore should succeed");
        let repeated = runner
            .capture_arm64_simd_fp_state()
            .expect("SIMD/FP state should be recaptured after repeated restore");
        assert!(
            repeated == state,
            "repeated SIMD/FP restore should preserve the complete state"
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn creates_hvf_gic_before_vcpu() {
    use bangbang_hvf::{HvfBackend, HvfGicMetadata};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    let metadata = *backend.create_gic().expect("GIC should be created");
    assert_eq!(metadata.msi, None);
    assert_eq!(HvfGicMetadata::FDT_COMPATIBILITY, "arm,gic-v3");
    assert!(metadata.distributor.size > 0);
    assert!(metadata.redistributor.region.size > 0);
    {
        let mut vcpu = backend
            .create_vcpu()
            .expect("vCPU should be created after GIC");
        vcpu.destroy().expect("vCPU should be destroyed");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn delivers_hvf_gic_msi_to_the_allocated_guest_intid() {
    use std::num::NonZeroU32;
    use std::sync::mpsc;
    use std::time::Duration;

    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfGicMsiConfiguration, HvfMemoryPermissions,
        HvfVcpuExit,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::fdt::ARM64_GICV2M_SPI_END_EXCLUSIVE;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    let metadata = *backend
        .create_gic_with_msi(HvfGicMsiConfiguration::new(
            NonZeroU32::new(1).expect("test MSI count should be nonzero"),
        ))
        .expect("MSI-enabled GIC should be created");
    let signaler = backend
        .gic_msi_signaler()
        .expect("MSI-enabled GIC should retain its sender")
        .clone();
    let interrupt = signaler
        .allocator()
        .allocate()
        .expect("one MSI should allocate");
    assert_eq!(
        interrupt.raw_value(),
        ARM64_GICV2M_SPI_END_EXCLUSIVE - 1,
        "the host terminal SPI should remain outside the GICv2m allocation"
    );
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let vector_base = guest_entry
        .checked_add(0x800)
        .expect("guest vector address should fit");
    let irq_handler = vector_base
        .checked_add(0x280)
        .expect("current-EL SPx IRQ vector should fit");
    let config_address = guest_entry
        .checked_add(0x1000)
        .expect("guest MSI config address should fit");
    let guest_code = GIC_MSI_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    let irq_code = GIC_MSI_IRQ_HANDLER
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("MSI guest setup code should be written");
    memory
        .write_slice(&irq_code, irq_handler)
        .expect("MSI guest IRQ handler should be written");

    let mut guest_config = Vec::with_capacity(32);
    guest_config.extend_from_slice(&metadata.distributor.base.to_le_bytes());
    guest_config.extend_from_slice(&metadata.redistributor.region.base.to_le_bytes());
    guest_config.extend_from_slice(&interrupt.raw_value().to_le_bytes());
    guest_config.extend_from_slice(&0_u32.to_le_bytes());
    guest_config.extend_from_slice(&vector_base.raw_value().to_le_bytes());
    memory
        .write_slice(&guest_config, config_address)
        .expect("MSI guest configuration should be written");

    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: config_address,
            })
            .expect("MSI guest boot registers should be configured");

        let HvfVcpuExit::Exception(ready) = runner
            .run_once()
            .expect("MSI guest should publish readiness through HVC")
        else {
            panic!("MSI guest readiness should produce an exception exit");
        };
        assert_eq!(
            ready
                .decode_hvc()
                .expect("MSI guest readiness should decode as HVC")
                .immediate(),
            0
        );
        let ready_registers = runner
            .capture_arm64_general_register_state()
            .expect("MSI readiness registers should be captured");
        assert_eq!(
            ready_registers
                .general_purpose_register(1)
                .expect("X1 should contain GICD_TYPER")
                & (1 << 17),
            0,
            "the validated GICv2m path requires a distributor without LPIs",
        );

        signaler
            .send(&interrupt)
            .expect("real Hypervisor.framework MSI should be sent");
        let cancel = runner.run_cancel_handle();
        let delivered = std::thread::scope(|scope| {
            let (sender, receiver) = mpsc::sync_channel(1);
            let runner_ref = &runner;
            scope.spawn(move || {
                let _ = sender.send(runner_ref.run_once());
            });

            match receiver.recv_timeout(Duration::from_secs(5)) {
                Ok(result) => result.expect("MSI guest run should succeed"),
                Err(error) => {
                    cancel
                        .cancel()
                        .expect("timed-out MSI guest run should cancel");
                    let _ = receiver.recv_timeout(Duration::from_secs(5));
                    panic!("MSI guest did not observe an IRQ before the deadline: {error}");
                }
            }
        });
        let HvfVcpuExit::Exception(delivered) = delivered else {
            panic!("delivered MSI should produce an exception exit");
        };
        assert_eq!(
            delivered
                .decode_hvc()
                .expect("MSI IRQ handler exit should decode as HVC")
                .immediate(),
            1
        );
        let registers = runner
            .capture_arm64_general_register_state()
            .expect("MSI IRQ result registers should be captured");
        assert_eq!(
            registers.general_purpose_register(0),
            Some(u64::from(interrupt.raw_value()))
        );

        runner
            .shutdown()
            .expect("MSI guest runner should shut down");
    }
    drop(signaler);
    backend
        .destroy_vm()
        .expect("MSI guest VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_hvf_gic_device_and_icc_state_before_run() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend.create_gic().expect("GIC should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");

        let state = runner
            .capture_gic_device_state()
            .expect("opaque GIC device state should be captured");
        assert!(!state.is_empty());
        assert_eq!(state.as_bytes().len(), state.len());
        let icc_state = runner
            .capture_arm64_gic_icc_register_state()
            .expect("GIC ICC register state should be captured before run");
        runner
            .restore_gic_device_state(&state)
            .expect("opaque GIC device state should be restored before run");
        for _ in 0..2 {
            runner
                .restore_arm64_gic_icc_register_state(&icc_state)
                .expect("GIC ICC register state should be restored before run");
            let restored_icc_state = runner
                .capture_arm64_gic_icc_register_state()
                .expect("restored GIC ICC register state should be recaptured");
            assert!(
                restored_icc_state == icc_state,
                "restored GIC ICC register state should match the original"
            );
        }

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_gic_icc_registers_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = GIC_ICC_REGISTER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("GIC ICC register guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend.create_gic().expect("GIC should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest GIC ICC writer should exit through HVC")
        else {
            panic!("guest GIC ICC writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest GIC ICC writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_gic_icc_register_state()
            .expect("GIC ICC register state should be captured");
        assert_eq!(state.pmr_el1(), GIC_ICC_TEST_PMR_EL1);
        assert_eq!(state.bpr0_el1(), GIC_ICC_TEST_BPR0_EL1);
        assert_eq!(state.bpr1_el1(), GIC_ICC_TEST_BPR1_EL1);
        assert_eq!(state.sre_el1() & 1, 1);
        assert_eq!(state.igrpen0_el1(), 1);
        assert_eq!(state.igrpen1_el1(), 1);
        let _host_defined_values = (
            state.ap0r0_el1(),
            state.ap1r0_el1(),
            state.rpr_el1(),
            state.ctlr_el1(),
        );

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn rejects_hvf_gic_after_vcpu_creation() {
    use bangbang_hvf::{HvfBackend, HvfGicError};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let mut vcpu = backend.create_vcpu().expect("vCPU should be created");
        vcpu.destroy().expect("vCPU should be destroyed");
    }
    assert_eq!(
        backend
            .create_gic()
            .expect_err("GIC creation after vCPU creation should fail"),
        HvfGicError::InvalidState("GIC must be created before creating vCPUs")
    );
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn cancels_runner_before_first_run() {
    use bangbang_hvf::{HvfBackend, HvfVcpuExit};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner.cancel().expect("runner should accept cancellation");
        assert_eq!(
            runner.run_once().expect("runner should return an exit"),
            HvfVcpuExit::Canceled
        );
        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn owns_and_cleans_ordered_two_vcpu_topology() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    backend
        .create_gic()
        .expect("GIC should be created before the vCPU topology");
    {
        let topology = backend
            .start_vcpu_topology(2)
            .expect("host should support a two-vCPU topology");
        assert_eq!(topology.mpidrs(), [0, 1]);
        assert_eq!(topology.len(), 2);

        topology
            .cancel()
            .expect("every topology member should accept cancellation");

        topology
            .shutdown()
            .expect("every topology member should shut down");
        topology
            .shutdown()
            .expect("topology shutdown should be idempotent");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn concurrently_runs_and_batch_cancels_two_vcpus() {
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuRunControlReason,
        HvfVcpuRunEvent, HvfVcpuRunMemberOutcome, HvfVcpuRunStepOutcome,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};
    use bangbang_runtime::mmio::MmioDispatcher;

    const SECOND_ENTRY_OFFSET: u64 = 0x100;
    const FLAGS_OFFSET: u64 = 0x2000;
    const PEER_FLAG_OFFSET: u32 = 8;
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
    const MOV_W1_ONE: u32 = 0x5280_0021;
    const STR_W1_X0: u32 = 0xb900_0001;
    const DMB_ISH: u32 = 0xd503_3bbf;
    const ADD_X2_X0_PEER: u32 = 0x9100_2002;
    const SUB_X2_X0_PEER: u32 = 0xd100_2002;
    const LDR_W3_X2: u32 = 0xb940_0043;
    const CBZ_W3_PREVIOUS: u32 = 0x34ff_ffe3;
    const SPIN_FOREVER: u32 = 0x1400_0000;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");

    for iteration in 0..2 {
        let mut backend = HvfBackend::new();
        let layout =
            aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
                .expect("guest memory layout should be valid");
        let mut memory =
            GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
        let first_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
        let second_entry = first_entry
            .checked_add(SECOND_ENTRY_OFFSET)
            .expect("second guest entry should fit");
        let first_flag = first_entry
            .checked_add(FLAGS_OFFSET)
            .expect("first handshake flag should fit");
        let second_flag = first_flag
            .checked_add(u64::from(PEER_FLAG_OFFSET))
            .expect("second handshake flag should fit");
        let first_code = [
            MOV_W1_ONE,
            STR_W1_X0,
            DMB_ISH,
            ADD_X2_X0_PEER,
            LDR_W3_X2,
            CBZ_W3_PREVIOUS,
            SPIN_FOREVER,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
        let second_code = [
            MOV_W1_ONE,
            STR_W1_X0,
            DMB_ISH,
            SUB_X2_X0_PEER,
            LDR_W3_X2,
            CBZ_W3_PREVIOUS,
            SPIN_FOREVER,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
        memory
            .write_slice(&first_code, first_entry)
            .expect("first guest handshake should be written");
        memory
            .write_slice(&second_code, second_entry)
            .expect("second guest handshake should be written");
        memory
            .write_slice(&[0; 16], first_flag)
            .expect("guest handshake flags should be zeroed");
        let dram_region = memory
            .regions()
            .first()
            .expect("guest DRAM should contain one region");
        assert_eq!(dram_region.range().start(), first_entry);
        let first_flag_host = dram_region
            .host_address()
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(FLAGS_OFFSET as usize)
            .cast::<u32>();
        let second_flag_host = first_flag_host.wrapping_add(2);

        backend.create_vm().expect("VM should be created");
        backend
            .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
            .expect("guest handshake memory should be mapped");
        backend
            .create_gic()
            .expect("GIC should be created before the vCPU topology");
        {
            let topology = backend
                .start_vcpu_topology(2)
                .expect("host should support a two-vCPU topology");
            let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
            let mut coordinator = topology
                .into_run_coordinator(dispatcher, &[0, 1])
                .expect("two-vCPU coordinator should start");
            coordinator
                .configure_arm64_boot_registers(
                    0,
                    HvfArm64BootRegisters {
                        kernel_entry: first_entry,
                        fdt_address: first_flag,
                    },
                )
                .expect("first guest entry should be configured");
            coordinator
                .configure_arm64_boot_registers(
                    1,
                    HvfArm64BootRegisters {
                        kernel_entry: second_entry,
                        fdt_address: second_flag,
                    },
                )
                .expect("second guest entry should be configured");
            assert_eq!(
                coordinator.dispatch_online(),
                Ok(2),
                "iteration {iteration} should submit both vCPUs before collection"
            );

            let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
            loop {
                // SAFETY: both aligned pointers remain inside the mapped DRAM
                // region owned by `backend`; volatile reads observe guest writes
                // while both vCPU owner threads are running.
                let flags = unsafe {
                    (
                        std::ptr::read_volatile(first_flag_host),
                        std::ptr::read_volatile(second_flag_host),
                    )
                };
                if flags == (1, 1) {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "iteration {iteration} timed out waiting for both guest handshake flags; observed {flags:?}"
                );
                std::thread::yield_now();
            }

            let waiter = coordinator
                .control()
                .request_stop()
                .expect("one active-only batch stop should start");
            let event = coordinator
                .receive_event()
                .expect("both canceled generations should drain");
            let HvfVcpuRunEvent::Barrier(report) = event else {
                panic!("iteration {iteration} should complete a stop barrier");
            };
            assert_eq!(report.reason(), HvfVcpuRunControlReason::Stop);
            assert_eq!(
                report
                    .acknowledgements()
                    .iter()
                    .map(|result| result.index())
                    .collect::<Vec<_>>(),
                vec![0, 1]
            );
            assert!(report.acknowledgements().iter().all(|result| matches!(
                result.result(),
                Ok(HvfVcpuRunMemberOutcome::Handled(
                    HvfVcpuRunStepOutcome::Canceled
                ))
            )));
            assert_eq!(waiter.wait(), Ok(report));

            coordinator
                .shutdown()
                .expect("coordinator should shut down every owner");
            coordinator
                .shutdown()
                .expect("coordinator shutdown should be idempotent");
        }
        backend
            .destroy_vm()
            .expect("VM teardown should unmap guest memory after owner shutdown");
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn tracks_concurrent_guest_writes_with_exact_retry_and_bounded_cancellation() {
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfDirtyWriteTrackerStopError, HvfMemoryPermissions,
        HvfVcpuRunControlReason, HvfVcpuRunEvent, HvfVcpuRunMemberOutcome, HvfVcpuRunStepOutcome,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::fdt::{Arm64FdtRegion, Arm64FdtVmGenIdDevice};
    use bangbang_runtime::interrupt::GuestInterruptLine;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};
    use bangbang_runtime::mmio::MmioDispatcher;
    use bangbang_runtime::startup::{
        ARM64_BOOT_VMGENID_SIZE, Arm64BootVmGenIdDevice, replace_arm64_boot_vmgenid,
    };

    const SECOND_ENTRY_OFFSET: u64 = 0x100;
    const TARGET_START_PAGE: u64 = 2;
    const VCPU0_VALUE: u16 = 0x11;
    const VCPU1_VALUE: u16 = 0x22;
    const VCPU0_SECOND_VALUE: u16 = 0x33;
    const VCPU1_SECOND_VALUE: u16 = 0x44;
    const MOV_X3_X0: u32 = 0xaa00_03e3;
    const STR_W1_X3: u32 = 0xb900_0061;
    const STR_W1_X2: u32 = 0xb900_0041;
    const DMB_ISH: u32 = 0xd503_3bbf;
    const HVC_ZERO: u32 = 0xd400_0002;
    const SPIN_FOREVER: u32 = 0x1400_0000;
    const MAX_MEMBER_EVENTS: usize = 16;
    const DIRTY_PROGRESS_TIMEOUT: Duration = Duration::from_secs(5);

    fn mov_w1(value: u16) -> u32 {
        0x5280_0001 | (u32::from(value) << 5)
    }

    fn add_x2_x3_page_offset(host_page_size: u64, pages: u64) -> u32 {
        const ADD_X2_X3_SHIFT_12: u32 = 0x9140_0062;
        const ARM64_IMMEDIATE_PAGE_SIZE: u64 = 0x1000;

        assert!(host_page_size.is_multiple_of(ARM64_IMMEDIATE_PAGE_SIZE));
        let immediate = host_page_size
            .checked_div(ARM64_IMMEDIATE_PAGE_SIZE)
            .and_then(|units| units.checked_mul(pages))
            .and_then(|units| u32::try_from(units).ok())
            .expect("guest page offset should fit the ADD immediate");
        assert!(immediate <= 0xfff);
        ADD_X2_X3_SHIFT_12 | (immediate << 10)
    }

    fn guest_code(
        host_page_size: u64,
        first_value: u16,
        second_value: u16,
        first_page: u64,
    ) -> Vec<u8> {
        [
            MOV_X3_X0,
            mov_w1(first_value),
            STR_W1_X3,
            add_x2_x3_page_offset(host_page_size, first_page),
            STR_W1_X2,
            add_x2_x3_page_offset(host_page_size, first_page + 1),
            STR_W1_X2,
            DMB_ISH,
            HVC_ZERO,
            mov_w1(second_value),
            STR_W1_X3,
            add_x2_x3_page_offset(host_page_size, first_page),
            STR_W1_X2,
            add_x2_x3_page_offset(host_page_size, first_page + 1),
            STR_W1_X2,
            DMB_ISH,
            HVC_ZERO,
            SPIN_FOREVER,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
    }

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let page_size = host_page_size().expect("host page size should be valid");
    let layout = aarch64::dram_layout(page_size * 8)
        .expect("dirty-write guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("dirty-write guest memory allocation should succeed");
    let first_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let second_entry = first_entry
        .checked_add(SECOND_ENTRY_OFFSET)
        .expect("second guest entry should fit");
    let shared_page = first_entry
        .checked_add(page_size * TARGET_START_PAGE)
        .expect("shared dirty page should fit");
    let vcpu0_pages = [
        shared_page
            .checked_add(page_size)
            .expect("first vCPU0 page should fit"),
        shared_page
            .checked_add(page_size * 2)
            .expect("second vCPU0 page should fit"),
    ];
    let vcpu1_pages = [
        shared_page
            .checked_add(page_size * 3)
            .expect("first vCPU1 page should fit"),
        shared_page
            .checked_add(page_size * 4)
            .expect("second vCPU1 page should fit"),
    ];
    let device_page = shared_page
        .checked_add(page_size * 5)
        .expect("current-device dirty page should fit");
    memory
        .write_slice(
            &guest_code(page_size, VCPU0_VALUE, VCPU0_SECOND_VALUE, 1),
            first_entry,
        )
        .expect("first dirty-write guest code should be written");
    memory
        .write_slice(
            &guest_code(page_size, VCPU1_VALUE, VCPU1_SECOND_VALUE, 3),
            second_entry,
        )
        .expect("second dirty-write guest code should be written");
    for page in [
        shared_page,
        vcpu0_pages[0],
        vcpu0_pages[1],
        vcpu1_pages[0],
        vcpu1_pages[1],
    ] {
        memory
            .write_slice(&0_u32.to_le_bytes(), page)
            .expect("dirty-write target should be zeroed");
    }
    let userspace_tracker = memory
        .enable_dirty_tracking()
        .expect("shared dirty epoch should start before current-device activity");
    let vmgenid_range = bangbang_runtime::memory::GuestMemoryRange::new(
        device_page,
        ARM64_BOOT_VMGENID_SIZE as u64,
    )
    .expect("current-device VMGenID range should validate");
    let mut vmgenid = Arm64BootVmGenIdDevice {
        range: vmgenid_range,
        generation_id: [0; ARM64_BOOT_VMGENID_SIZE],
        fdt_device: Arm64FdtVmGenIdDevice {
            region: Arm64FdtRegion {
                base: device_page.raw_value(),
                size: ARM64_BOOT_VMGENID_SIZE as u64,
            },
            interrupt_line: GuestInterruptLine::new(1)
                .expect("current-device interrupt line should validate"),
        },
    };
    replace_arm64_boot_vmgenid(&mut memory, &mut vmgenid)
        .expect("current VMGenID device should write through tracked guest memory");
    assert_eq!(
        userspace_tracker
            .dirty_pages()
            .expect("current-device dirty page should query"),
        vec![device_page]
    );
    let dram_region = memory
        .regions()
        .first()
        .expect("dirty-write guest DRAM should contain one region");
    let target_host_pointer = |address: GuestAddress| {
        let offset = address
            .raw_value()
            .checked_sub(dram_region.range().start().raw_value())
            .and_then(|offset| usize::try_from(offset).ok())
            .expect("dirty-write target offset should fit this host");
        dram_region
            .host_address()
            .as_ptr()
            .cast::<u8>()
            .wrapping_add(offset)
            .cast::<u32>()
    };
    let shared_host = target_host_pointer(shared_page);
    let vcpu0_hosts = vcpu0_pages.map(target_host_pointer);
    let vcpu1_hosts = vcpu1_pages.map(target_host_pointer);

    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("dirty-write guest memory should be mapped");
    let tracker = backend
        .start_dirty_write_tracking()
        .expect("dirty-write tracking should start before vCPU ownership");
    backend
        .create_gic()
        .expect("GIC should be created before the tracked vCPU topology");
    {
        let topology = backend
            .start_vcpu_topology(2)
            .expect("host should support a tracked two-vCPU topology");
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        let mut coordinator = topology
            .into_run_coordinator(dispatcher, &[0, 1])
            .expect("tracked two-vCPU coordinator should start");
        for (index, entry) in [first_entry, second_entry].into_iter().enumerate() {
            coordinator
                .configure_arm64_boot_registers(
                    index,
                    HvfArm64BootRegisters {
                        kernel_entry: entry,
                        fdt_address: shared_page,
                    },
                )
                .expect("tracked guest entry should be configured");
        }
        assert_eq!(coordinator.dispatch_online(), Ok(2));
        let watchdog_control = coordinator.control();
        let (progress_sender, progress_receiver) = mpsc::channel();
        let watchdog = std::thread::spawn(move || {
            if progress_receiver
                .recv_timeout(DIRTY_PROGRESS_TIMEOUT)
                .is_err()
            {
                let _ = watchdog_control.request_stop();
            }
        });

        let expected_pages = [
            shared_page,
            vcpu0_pages[0],
            vcpu0_pages[1],
            vcpu1_pages[0],
            vcpu1_pages[1],
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let expected_vcpu0_pages = vcpu0_pages.into_iter().collect::<BTreeSet<_>>();
        let expected_vcpu1_pages = vcpu1_pages.into_iter().collect::<BTreeSet<_>>();
        for epoch_index in 0..2u64 {
            let mut first_write_pages = BTreeSet::new();
            let mut vcpu0_first_write_pages = BTreeSet::new();
            let mut vcpu1_first_write_pages = BTreeSet::new();
            let mut stale_shared_faults = 0usize;
            let mut reached_hvc = [false; 2];

            for _ in 0..MAX_MEMBER_EVENTS {
                let event = coordinator
                    .receive_event()
                    .expect("tracked member event should be received");
                let HvfVcpuRunEvent::Member(result) = event else {
                    panic!("tracked guest should not terminate before cancellation: {event:?}");
                };
                match result.result() {
                    Ok(HvfVcpuRunMemberOutcome::Handled(HvfVcpuRunStepOutcome::DirtyWrite {
                        page,
                        first_write,
                    })) => {
                        assert!(expected_pages.contains(page));
                        if *first_write {
                            assert!(first_write_pages.insert(*page));
                            match result.index() {
                                0 => {
                                    vcpu0_first_write_pages.insert(*page);
                                }
                                1 => {
                                    vcpu1_first_write_pages.insert(*page);
                                }
                                index => panic!("unexpected tracked member index {index}"),
                            }
                        } else {
                            assert_eq!(*page, shared_page);
                            stale_shared_faults += 1;
                            assert!(stale_shared_faults <= 1);
                        }
                        assert_eq!(
                            coordinator.dispatch_online(),
                            Ok(1),
                            "a dirty exit should retry exactly the completed member"
                        );
                    }
                    Ok(HvfVcpuRunMemberOutcome::Handled(HvfVcpuRunStepOutcome::Hvc { .. })) => {
                        let index = result.index();
                        reached_hvc[index] = true;
                        coordinator
                            .set_online(index, false)
                            .expect("an idle epoch-complete member should go offline");
                    }
                    outcome => panic!("unexpected tracked member outcome: {outcome:?}"),
                }
                if reached_hvc == [true, true] {
                    break;
                }
            }

            assert_eq!(reached_hvc, [true, true]);
            assert_eq!(first_write_pages, expected_pages);
            assert!(expected_vcpu0_pages.is_subset(&vcpu0_first_write_pages));
            assert!(expected_vcpu1_pages.is_subset(&vcpu1_first_write_pages));
            let mut expected_epoch_pages = expected_pages.clone();
            if epoch_index == 0 {
                expected_epoch_pages.insert(device_page);
            }
            assert_eq!(
                tracker
                    .dirty_pages()
                    .expect("active tracker query should succeed")
                    .into_iter()
                    .collect::<BTreeSet<_>>(),
                expected_epoch_pages
            );

            // SAFETY: these aligned pointers remain inside the live mapped
            // DRAM region owned by `backend`; both HVC exits follow a DMB.
            let (shared_value, vcpu0_values, vcpu1_values) = unsafe {
                (
                    std::ptr::read_volatile(shared_host),
                    vcpu0_hosts.map(|pointer| std::ptr::read_volatile(pointer)),
                    vcpu1_hosts.map(|pointer| std::ptr::read_volatile(pointer)),
                )
            };
            let (vcpu0_value, vcpu1_value) = if epoch_index == 0 {
                (VCPU0_VALUE, VCPU1_VALUE)
            } else {
                (VCPU0_SECOND_VALUE, VCPU1_SECOND_VALUE)
            };
            assert!([u32::from(vcpu0_value), u32::from(vcpu1_value)].contains(&shared_value));
            assert_eq!(vcpu0_values, [u32::from(vcpu0_value); 2]);
            assert_eq!(vcpu1_values, [u32::from(vcpu1_value); 2]);

            assert_eq!(tracker.reset_epoch_quiesced(), Ok(epoch_index + 1));
            assert!(
                tracker
                    .dirty_pages()
                    .expect("advanced epoch should be clean")
                    .is_empty()
            );
            for index in 0..2 {
                coordinator
                    .set_online(index, true)
                    .expect("an idle epoch-complete member should return online");
            }
            if epoch_index == 0 {
                assert_eq!(
                    coordinator.dispatch_online(),
                    Ok(2),
                    "both idle owners should enter the second protected epoch"
                );
            }
        }

        progress_sender
            .send(())
            .expect("dirty progress watchdog should be released");
        watchdog
            .join()
            .expect("dirty progress watchdog should join");
        assert_eq!(
            tracker.stop(),
            Err(HvfDirtyWriteTrackerStopError::OwnersActive { count: 2 })
        );
        assert_eq!(
            coordinator.dispatch_online(),
            Ok(2),
            "both owners should resume into the bounded cancellation target"
        );

        let waiter = coordinator
            .control()
            .request_stop()
            .expect("tracked active runs should accept aggregate cancellation");
        let event = coordinator
            .receive_event()
            .expect("tracked cancellation barrier should drain");
        let HvfVcpuRunEvent::Barrier(report) = event else {
            panic!("tracked cancellation should complete a barrier: {event:?}");
        };
        assert_eq!(report.reason(), HvfVcpuRunControlReason::Stop);
        assert!(report.acknowledgements().iter().all(|result| matches!(
            result.result(),
            Ok(HvfVcpuRunMemberOutcome::Handled(
                HvfVcpuRunStepOutcome::Canceled
            ))
        )));
        assert_eq!(waiter.wait(), Ok(report));
        assert_eq!(
            tracker.stop(),
            Err(HvfDirtyWriteTrackerStopError::OwnersActive { count: 2 })
        );

        coordinator
            .shutdown()
            .expect("tracked coordinator should shut down every owner");
        tracker
            .stop()
            .expect("owner-free tracker should restore remaining clean ranges");
    }
    backend
        .stop_dirty_write_tracking()
        .expect("backend tracker retention should clear idempotently");
    backend
        .destroy_vm()
        .expect("tracked VM should unmap after owner shutdown");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_arm64_physical_timer_tval_on_idle_runner_thread() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend
        .create_gic()
        .expect("GIC should be created before the physical-timer vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let first = runner
            .capture_arm64_physical_timer_state()
            .expect("first idle physical-timer state should be captured");
        let second = runner
            .capture_arm64_physical_timer_state()
            .expect("second idle physical-timer state should be captured");

        let _first_tval = first.cntp_tval_el0();
        let _second_tval = second.cntp_tval_el0();

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_guest_written_arm64_physical_timer_state_on_runner_thread() {
    use bangbang_hvf::{HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuExit};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let mut memory =
        GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_code = PHYSICAL_TIMER_GUEST_CODE
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("physical-timer guest code should be written");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend
        .create_gic()
        .expect("GIC should be created before the physical-timer vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_entry,
            })
            .expect("guest code boot registers should be configured");

        let HvfVcpuExit::Exception(exit) = runner
            .run_once()
            .expect("guest physical-timer writer should exit through HVC")
        else {
            panic!("guest physical-timer writer should produce an exception exit");
        };
        assert_eq!(
            exit.decode_hvc()
                .expect("guest physical-timer writer exit should decode as HVC")
                .immediate(),
            0
        );

        let state = runner
            .capture_arm64_physical_timer_state()
            .expect("physical-timer state should be captured");
        assert_eq!(state.cntkctl_el1(), PHYSICAL_TIMER_TEST_CNTKCTL_EL1);
        assert_eq!(
            state.cntp_ctl_el0() & PHYSICAL_TIMER_WRITABLE_CONTROL_MASK,
            PHYSICAL_TIMER_TEST_CNTP_CTL_EL0
        );
        assert_eq!(
            state.cntp_ctl_el0() & !PHYSICAL_TIMER_DEFINED_CONTROL_MASK,
            0
        );
        assert!(matches!(
            state.cntp_ctl_el0() & PHYSICAL_TIMER_ISTATUS_MASK,
            0 | PHYSICAL_TIMER_ISTATUS_MASK
        ));
        assert_eq!(state.cntp_cval_el0(), PHYSICAL_TIMER_TEST_CNTP_CVAL_EL0);

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_runner_arm64_virtual_timer_state() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let original = runner
            .capture_arm64_virtual_timer_state()
            .expect("original runner vtimer state should be captured");

        runner
            .set_vtimer_mask(true)
            .expect("runner vtimer mask should be set");
        runner
            .set_vtimer_control(0)
            .expect("runner vtimer should be disabled");
        runner
            .set_vtimer_offset(VTIMER_TEST_OFFSET)
            .expect("runner vtimer offset should be set");
        runner
            .set_vtimer_compare_value(VTIMER_TEST_COMPARE_VALUE)
            .expect("runner vtimer compare value should be set");

        let captured = runner
            .capture_arm64_virtual_timer_state()
            .expect("runner vtimer state should be captured");
        assert!(captured.masked());
        assert_eq!(captured.offset(), VTIMER_TEST_OFFSET);
        assert_eq!(captured.control() & VTIMER_WRITABLE_CONTROL_MASK, 0);
        assert_eq!(captured.compare_value(), VTIMER_TEST_COMPARE_VALUE);

        runner
            .set_vtimer_offset(original.offset())
            .expect("original runner vtimer offset should be restored");
        runner
            .set_vtimer_compare_value(original.compare_value())
            .expect("original runner vtimer compare value should be restored");
        runner
            .set_vtimer_control(original.control() & VTIMER_WRITABLE_CONTROL_MASK)
            .expect("original runner vtimer control should be restored");
        runner
            .set_vtimer_mask(original.masked())
            .expect("original runner vtimer mask should be restored");

        let restored = runner
            .capture_arm64_virtual_timer_state()
            .expect("restored runner vtimer state should be captured");
        assert_eq!(restored.masked(), original.masked());
        assert_eq!(restored.offset(), original.offset());
        assert_eq!(
            restored.control() & VTIMER_WRITABLE_CONTROL_MASK,
            original.control() & VTIMER_WRITABLE_CONTROL_MASK
        );
        assert_eq!(restored.compare_value(), original.compare_value());

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn waits_for_retained_runner_virtual_timer_and_drains_control() {
    use std::time::{Duration, Instant};

    use bangbang_hvf::{HvfBackend, HvfVcpuRetainedVtimerWaitOutcome, HvfVcpuRunner};
    use bangbang_runtime::VmBackend;

    fn wait_for_admission(runner: &HvfVcpuRunner<'_>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if runner
                .retained_vtimer_wait_active()
                .expect("retained-wait activity should be observable")
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "retained wait did not publish its admission"
            );
            std::thread::yield_now();
        }
    }

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    let metadata = *backend.create_gic().expect("GIC should be created");
    let virtual_timer_intid = metadata.timer_interrupts.el1_virtual_timer_intid;
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        let original = runner
            .capture_arm64_virtual_timer_state()
            .expect("original runner vtimer state should be captured");

        for exit_masked in [false, true] {
            runner
                .set_vtimer_control(0)
                .expect("vtimer should be disabled before programming");
            runner
                .set_vtimer_mask(exit_masked)
                .expect("vtimer exit mask should be selected");
            let deadline = mach_counter_sample().wrapping_add(
                mach_ticks_for(Duration::from_millis(100))
                    .expect("test Mach deadline should fit u64"),
            );
            runner
                .set_vtimer_compare_value(deadline.wrapping_sub(original.offset()))
                .expect("future vtimer comparator should be programmed");
            runner
                .set_vtimer_control(1)
                .expect("future vtimer should be enabled and guest-unmasked");

            assert_eq!(
                runner.wait_for_retained_vtimer(virtual_timer_intid),
                Ok(HvfVcpuRetainedVtimerWaitOutcome::TimerPending)
            );
            let completed_at = mach_counter_sample();
            assert!(
                completed_at.wrapping_sub(deadline) < (1_u64 << 63),
                "retained wait returned before its real Mach deadline"
            );
            runner
                .clear_gic_ppi_pending(virtual_timer_intid)
                .expect("published timer PPI should clear");
        }

        runner
            .set_vtimer_control(0)
            .expect("vtimer should be disabled before programming a due comparator");
        runner
            .set_vtimer_mask(false)
            .expect("due vtimer exits should be unmasked");
        let due = mach_counter_sample();
        runner
            .set_vtimer_compare_value(due.wrapping_sub(original.offset()))
            .expect("due vtimer comparator should be programmed");
        runner
            .set_vtimer_control(1)
            .expect("due vtimer should be enabled and guest-unmasked");
        assert_eq!(
            runner.wait_for_retained_vtimer(virtual_timer_intid),
            Ok(HvfVcpuRetainedVtimerWaitOutcome::TimerPending)
        );
        runner
            .clear_gic_ppi_pending(virtual_timer_intid)
            .expect("due timer PPI should clear");

        for control in [0, 0b11] {
            runner
                .set_vtimer_control(control)
                .expect("indefinite retained timer control should be programmed");
            let cancel = runner.run_cancel_handle();
            std::thread::scope(|scope| {
                let wait = scope.spawn(|| runner.wait_for_retained_vtimer(virtual_timer_intid));
                wait_for_admission(&runner);
                cancel.cancel().expect("retained wait should cancel");
                assert_eq!(
                    wait.join().expect("wait caller should not panic"),
                    Ok(HvfVcpuRetainedVtimerWaitOutcome::Canceled)
                );
            });
        }

        runner
            .set_vtimer_control(0)
            .expect("shutdown retained timer should be disabled");
        std::thread::scope(|scope| {
            let wait = scope.spawn(|| runner.wait_for_retained_vtimer(virtual_timer_intid));
            wait_for_admission(&runner);
            runner
                .shutdown()
                .expect("shutdown should drain retained owner wait");
            assert_eq!(
                wait.join().expect("wait caller should not panic"),
                Ok(HvfVcpuRetainedVtimerWaitOutcome::Canceled)
            );
        });
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_normalized_arm64_timers_across_fresh_hvf_vms() {
    use bangbang_hvf::{HvfArm64SnapshotTimerState, HvfBackend};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("source VM should be created");
    backend
        .create_gic()
        .expect("source GIC should be created before its vCPU");
    let source = {
        let runner = backend
            .start_vcpu_runner()
            .expect("source vCPU runner should start");
        let state = runner
            .capture_arm64_snapshot_timer_state()
            .expect("source normalized timer state should be captured");
        runner.shutdown().expect("source runner should shut down");
        state
    };
    backend.destroy_vm().expect("source VM should be destroyed");

    backend
        .create_vm()
        .expect("fresh destination VM should be created");
    backend
        .create_gic()
        .expect("destination GIC should be created before its vCPU");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("destination vCPU runner should start");
        runner
            .restore_arm64_snapshot_timer_state(source)
            .expect("source timer state should restore on the fresh unrun vCPU");
        let recaptured = runner
            .capture_arm64_snapshot_timer_state()
            .expect("destination timer state should be recaptured");
        assert_normalized_timer_restore_equivalent(source, recaptured);

        let armed = HvfArm64SnapshotTimerState::try_new(
            true,
            3,
            recaptured.virtual_count(),
            0b11,
            recaptured.virtual_count().wrapping_add(10_000_000),
            0b11,
            10_000_000,
        )
        .expect("armed normalized timer state should be valid");
        runner
            .restore_arm64_snapshot_timer_state(armed)
            .expect("armed masked timer state should restore before first run");
        let recaptured_armed = runner
            .capture_arm64_snapshot_timer_state()
            .expect("armed timer state should be recaptured");
        assert_normalized_timer_restore_equivalent(armed, recaptured_armed);

        runner
            .shutdown()
            .expect("destination runner should shut down");
    }
    backend
        .destroy_vm()
        .expect("destination VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_and_restores_runner_arm64_pending_interrupt_state() {
    use bangbang_hvf::{HvfBackend, HvfInterruptType};
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");

        runner
            .set_pending_interrupt(HvfInterruptType::Irq, true)
            .expect("runner IRQ pending level should be set");
        runner
            .set_pending_interrupt(HvfInterruptType::Fiq, false)
            .expect("runner FIQ pending level should be cleared");
        let irq_only = runner
            .capture_arm64_pending_interrupt_state()
            .expect("IRQ-only pending state should be captured");
        assert!(irq_only.irq_pending());
        assert!(!irq_only.fiq_pending());

        runner
            .set_pending_interrupt(HvfInterruptType::Irq, false)
            .expect("runner IRQ pending level should be cleared");
        runner
            .set_pending_interrupt(HvfInterruptType::Fiq, true)
            .expect("runner FIQ pending level should be set");
        let fiq_only = runner
            .capture_arm64_pending_interrupt_state()
            .expect("FIQ-only pending state should be captured");
        assert!(!fiq_only.irq_pending());
        assert!(fiq_only.fiq_pending());

        runner
            .restore_arm64_pending_interrupt_state(&irq_only)
            .expect("IRQ-only pending state should be restored");
        let restored = runner
            .capture_arm64_pending_interrupt_state()
            .expect("restored pending-interrupt state should be captured");
        assert!(
            restored == irq_only,
            "restored pending-interrupt state should match its source"
        );
        runner
            .restore_arm64_pending_interrupt_state(&irq_only)
            .expect("IRQ-only pending state should be restored a second time");
        let restored_again = runner
            .capture_arm64_pending_interrupt_state()
            .expect("twice-restored pending-interrupt state should be captured");
        assert!(
            restored_again == irq_only,
            "twice-restored pending-interrupt state should match its source"
        );

        runner
            .set_pending_interrupt(HvfInterruptType::Irq, false)
            .expect("runner IRQ pending level should remain cleared");
        runner
            .set_pending_interrupt(HvfInterruptType::Fiq, false)
            .expect("runner FIQ pending level should be cleared");
        let cleared = runner
            .capture_arm64_pending_interrupt_state()
            .expect("cleared pending state should be captured");
        assert!(!cleared.irq_pending());
        assert!(!cleared.fiq_pending());

        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn sets_and_clears_runner_gic_ppi_pending() {
    use bangbang_hvf::HvfBackend;
    use bangbang_runtime::VmBackend;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();

    backend.create_vm().expect("VM should be created");
    let metadata = *backend.create_gic().expect("GIC should be created");
    let virtual_timer_intid = metadata.timer_interrupts.el1_virtual_timer_intid;
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("vCPU runner should start");
        runner
            .set_gic_ppi_pending(virtual_timer_intid)
            .expect("runner GIC PPI pending bit should be set");
        runner
            .clear_gic_ppi_pending(virtual_timer_intid)
            .expect("runner GIC PPI pending bit should be cleared");
        runner.shutdown().expect("runner should shut down");
    }
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn maps_guest_memory_and_unmaps_before_destroying_vm() {
    use bangbang_hvf::{HvfBackend, HvfMemoryPermissions};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestMemory, aarch64};

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let mut backend = HvfBackend::new();
    let layout = aarch64::dram_layout(host_page_size().expect("host page size should be valid"))
        .expect("guest memory layout should be valid");
    let memory = GuestMemory::allocate(&layout).expect("guest memory allocation should succeed");

    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("guest memory should be mapped");
    backend
        .destroy_vm()
        .expect("VM destruction should unmap guest memory first");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn maps_shared_guest_memory_and_exposes_guest_writes_through_its_descriptor() {
    use std::fs::File;
    use std::os::fd::AsFd;
    use std::os::unix::fs::FileExt;
    use std::sync::{Arc, Mutex};

    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuRunStepOutcome,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{
        GuestAddress, GuestMemory, GuestMemoryBacking, GuestMemoryRange, aarch64,
    };
    use bangbang_runtime::mmio::MmioDispatcher;

    const MOV_W1_TEST_VALUE: u32 = 0x5280_0b41;
    const STR_W1_X0: u32 = 0xb900_0001;
    const DMB_ISH: u32 = 0xd503_3bbf;
    const HVC_ZERO: u32 = 0xd400_0002;
    const TEST_VALUE: u32 = 0x5a;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let page_size = host_page_size().expect("host page size should be valid");
    let layout =
        aarch64::dram_layout(page_size * 2).expect("shared guest memory layout should be valid");
    let mut memory = GuestMemory::allocate_with_backing(&layout, GuestMemoryBacking::Shared)
        .expect("shared guest memory allocation should succeed");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_target = guest_entry
        .checked_add(page_size)
        .expect("guest write target should fit");
    let guest_code = [MOV_W1_TEST_VALUE, STR_W1_X0, DMB_ISH, HVC_ZERO]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
    memory
        .write_slice(&guest_code, guest_entry)
        .expect("shared guest code should be written");
    let export = memory.regions()[0]
        .try_clone_shared_backing()
        .expect("shared descriptor should clone")
        .expect("shared guest memory should expose a descriptor");
    let export_file = File::from(
        export
            .as_fd()
            .try_clone_to_owned()
            .expect("shared descriptor clone should be independent"),
    );

    let mut backend = HvfBackend::new();
    backend.create_vm().expect("VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("shared guest memory should be mapped");
    let dynamic_range = GuestMemoryRange::new(
        guest_entry
            .checked_add(page_size * 2)
            .expect("dynamic shared range should fit"),
        page_size,
    )
    .expect("dynamic shared range should validate");
    backend
        .map_dynamic_guest_memory_region(dynamic_range, HvfMemoryPermissions::GUEST_RAM)
        .expect("dynamic shared guest memory should map");
    let tracker = backend
        .start_dirty_write_tracking()
        .expect("shared guest memory should be write-protected");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("shared-memory vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_target,
            })
            .expect("shared-memory guest registers should configure");
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        assert_eq!(
            runner
                .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                .expect("first shared-memory write exit should be handled"),
            HvfVcpuRunStepOutcome::DirtyWrite {
                page: guest_target,
                first_write: true,
            }
        );
        assert!(matches!(
            runner
                .run_once_and_handle_mmio(dispatcher)
                .expect("retried shared-memory guest should reach HVC"),
            HvfVcpuRunStepOutcome::Hvc { exit, .. } if exit.immediate() == 0
        ));
        runner
            .shutdown()
            .expect("shared-memory vCPU runner should shut down");
    }
    assert_eq!(
        tracker
            .dirty_pages()
            .expect("shared-memory dirty pages should query"),
        vec![guest_target]
    );
    tracker
        .stop()
        .expect("owner-free shared-memory tracker should restore write access");
    backend
        .stop_dirty_write_tracking()
        .expect("shared-memory tracker retention should clear");

    let mut descriptor_value = [0_u8; std::mem::size_of::<u32>()];
    export_file
        .read_exact_at(&mut descriptor_value, page_size)
        .expect("shared descriptor should observe the guest write");
    assert_eq!(u32::from_le_bytes(descriptor_value), TEST_VALUE);

    backend
        .unmap_dynamic_guest_memory_region(dynamic_range)
        .expect("dynamic shared guest memory should unmap");
    backend
        .unmap_guest_memory()
        .expect("shared guest memory should unmap");
    backend.destroy_vm().expect("VM should be destroyed");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn maps_native_v2_file_memory_on_demand_with_cow_dirty_and_cleanup() {
    use std::fs::OpenOptions;
    use std::os::unix::fs::FileExt;
    use std::sync::{Arc, Mutex};

    use bangbang_hvf::{
        HvfArm64BootRegisters, HvfBackend, HvfMemoryPermissions, HvfVcpuRunStepOutcome,
    };
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, aarch64};
    use bangbang_runtime::mmio::MmioDispatcher;
    use bangbang_runtime::snapshot_format_v2::decode_snapshot_v2_state;
    use bangbang_runtime::snapshot_memory_v2::{
        encode_snapshot_v2_state_with_memory, load_snapshot_v2_memory_file,
        write_snapshot_v2_memory_image,
    };

    const IMAGE_BYTES: u64 = 64 * 1024 * 1024;
    const TARGET_OFFSET: u64 = IMAGE_BYTES - 64 * 1024;
    const TEST_VALUE: u32 = 0x5aa5_c33c;
    const INITIAL_TARGET: [u8; 4] = [0x13, 0x37, 0x42, 0x99];
    const LDR_W2_X0: u32 = 0xb940_0002;
    const HVC_ZERO: u32 = 0xd400_0002;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let layout = aarch64::dram_layout(IMAGE_BYTES).expect("large v2 layout should be valid");
    let mut source_memory = GuestMemory::allocate(&layout).expect("source memory should allocate");
    let guest_entry = GuestAddress::new(aarch64::DRAM_MEM_START);
    let guest_target = guest_entry
        .checked_add(TARGET_OFFSET)
        .expect("distant target should fit");
    let mut guest_code = arm64_store_u32_and_hvc_program(guest_target.raw_value(), TEST_VALUE);
    guest_code.truncate(guest_code.len() - std::mem::size_of::<u32>());
    guest_code.extend_from_slice(&LDR_W2_X0.to_le_bytes());
    guest_code.extend_from_slice(&HVC_ZERO.to_le_bytes());
    guest_code.extend_from_slice(&HVC_ZERO.to_le_bytes());
    source_memory
        .write_slice(&guest_code, guest_entry)
        .expect("v2 guest code should write");
    source_memory
        .write_slice(&INITIAL_TARGET, guest_target)
        .expect("v2 distant source value should write");

    let image_file = TempFile::new_len("native-v2-lazy-memory", 0)
        .expect("empty v2 memory artifact should create");
    let mut writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(image_file.path())
        .expect("v2 memory artifact should open for writing");
    let binding = write_snapshot_v2_memory_image(&source_memory, &mut writer)
        .expect("v2 memory artifact should write");
    drop(writer);
    drop(source_memory);

    let state_bytes =
        encode_snapshot_v2_state_with_memory(&binding).expect("v2 memory state should encode");
    let state = decode_snapshot_v2_state(&state_bytes).expect("v2 memory state should decode");
    let source_file =
        std::fs::File::open(image_file.path()).expect("v2 source should open read-only");
    let data_offset = binding.extents()[0].file_offset();
    let target_file_offset = data_offset + TARGET_OFFSET;
    let mut source_code_before = vec![0_u8; guest_code.len()];
    source_file
        .read_exact_at(&mut source_code_before, data_offset)
        .expect("source code should read before mapping");
    let mut source_target_before = [0_u8; 4];
    source_file
        .read_exact_at(&mut source_target_before, target_file_offset)
        .expect("source target should read before mapping");
    assert_eq!(source_code_before, guest_code);
    assert_eq!(source_target_before, INITIAL_TARGET);

    let before_load = process_memory_usage().expect("pre-load process usage should query");
    let memory =
        load_snapshot_v2_memory_file(&state, source_file).expect("v2 memory should map lazily");
    let after_load = process_memory_usage().expect("post-load process usage should query");
    let load_growth = after_load.saturating_growth_from(before_load);
    assert!(
        load_growth.virtual_size >= IMAGE_BYTES / 2,
        "lazy mapping should reserve most of the image virtually: {load_growth:?}"
    );
    assert!(
        load_growth.resident_size < IMAGE_BYTES / 2,
        "metadata validation must not make half the image resident: {load_growth:?}"
    );
    assert!(
        load_growth.faults < IMAGE_BYTES / host_page_size().expect("page size should query") / 2,
        "metadata validation must not fault half the image: {load_growth:?}"
    );

    let mut backend = HvfBackend::new();
    backend.create_vm().expect("v2 VM should be created");
    backend
        .map_guest_memory(memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("v2 file memory should map into HVF");
    let tracker = backend
        .start_dirty_write_tracking()
        .expect("v2 memory should establish a clean dirty baseline");
    assert!(
        tracker
            .dirty_pages()
            .expect("initial v2 dirty pages should query")
            .is_empty()
    );
    let before_guest = process_memory_usage().expect("pre-guest process usage should query");
    {
        let runner = backend
            .start_vcpu_runner()
            .expect("v2 memory vCPU runner should start");
        runner
            .configure_arm64_boot_registers(HvfArm64BootRegisters {
                kernel_entry: guest_entry,
                fdt_address: guest_target,
            })
            .expect("v2 memory guest registers should configure");
        let dispatcher = Arc::new(Mutex::new(MmioDispatcher::new()));
        assert_eq!(
            runner
                .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                .expect("v2 guest first write should be handled"),
            HvfVcpuRunStepOutcome::DirtyWrite {
                page: guest_target,
                first_write: true,
            }
        );
        assert!(matches!(
            runner
                .run_once_and_handle_mmio(Arc::clone(&dispatcher))
                .expect("v2 guest should retry through first HVC"),
            HvfVcpuRunStepOutcome::Hvc { exit, .. } if exit.immediate() == 0
        ));
        assert_eq!(
            runner
                .capture_arm64_general_register_state()
                .expect("v2 guest registers should capture")
                .general_purpose_register(2),
            Some(u64::from(TEST_VALUE))
        );
        assert!(matches!(
            runner
                .run_once_and_handle_mmio(dispatcher)
                .expect("v2 guest should continue through second HVC"),
            HvfVcpuRunStepOutcome::Hvc { exit, .. } if exit.immediate() == 0
        ));
        runner
            .shutdown()
            .expect("v2 memory vCPU runner should shut down");
    }
    let after_guest = process_memory_usage().expect("post-guest process usage should query");
    assert!(
        after_guest.faults > before_guest.faults || after_guest.pageins > before_guest.pageins,
        "first guest access should add a demand fault or page-in: before={before_guest:?}, after={after_guest:?}"
    );
    assert_eq!(
        tracker
            .dirty_pages()
            .expect("v2 memory dirty pages should query"),
        vec![guest_target]
    );
    tracker
        .stop()
        .expect("v2 dirty tracker should restore write access");
    backend
        .stop_dirty_write_tracking()
        .expect("v2 tracker retention should clear");

    let source_file =
        std::fs::File::open(image_file.path()).expect("v2 source should reopen for verification");
    let mut source_code_after = vec![0_u8; guest_code.len()];
    source_file
        .read_exact_at(&mut source_code_after, data_offset)
        .expect("source code should read after execution");
    let mut source_target_after = [0_u8; 4];
    source_file
        .read_exact_at(&mut source_target_after, target_file_offset)
        .expect("source target should read after execution");
    assert_eq!(source_code_after, source_code_before);
    assert_eq!(source_target_after, source_target_before);

    backend
        .unmap_guest_memory()
        .expect("v2 memory should unmap ordinarily");
    backend.destroy_vm().expect("v2 VM should destroy");

    let fallback_memory = load_snapshot_v2_memory_file(
        &state,
        std::fs::File::open(image_file.path())
            .expect("v2 source should reopen for fallback cleanup"),
    )
    .expect("second v2 memory should map lazily");
    let mut fallback_backend = HvfBackend::new();
    fallback_backend
        .create_vm()
        .expect("fallback v2 VM should create");
    fallback_backend
        .map_guest_memory(fallback_memory, HvfMemoryPermissions::GUEST_RAM)
        .expect("fallback v2 memory should map");
    fallback_backend
        .destroy_vm()
        .expect("VM destroy should release v2 memory after backend unmap");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn prepares_internal_hvf_arm64_boot_session() {
    use bangbang_hvf::{ARM64_LINUX_BOOT_CPSR, HvfArm64BootSessionConfig, HvfBackend};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{PmemConfigInput, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT};
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("session-kernel", &image).expect("temp kernel should be created");
    let writable_pmem = TempFile::new_len("session-writable-pmem", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp writable pmem should be created");
    let readonly_pmem = TempFile::new_len("session-readonly-pmem", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp readonly pmem should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
            "pmem0",
            path_text(writable_pmem.path()),
        )))
        .expect("writable pmem config should be stored");
    controller
        .handle_action(VmmAction::PutPmem(
            PmemConfigInput::new("pmem1", path_text(readonly_pmem.path())).with_read_only(true),
        ))
        .expect("readonly pmem config should be stored");
    let mut backend = HvfBackend::new();
    let pmem_mmio_layout =
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500));
    let rtc_mmio_layout = test_rtc_mmio_layout();
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        pmem_mmio_layout,
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        rtc_mmio_layout,
    );

    let mut session = backend
        .prepare_arm64_boot_session(&controller, config.clone())
        .expect("internal HVF arm64 boot session should prepare");

    let mmio_dispatcher = session.mmio_dispatcher();
    let mmio_regions = mmio_dispatcher
        .try_lock()
        .expect("session MMIO dispatcher should lock")
        .regions()
        .to_vec();
    assert_eq!(mmio_regions.len(), 3);
    let first_pmem_region = mmio_regions
        .iter()
        .find(|region| region.id() == pmem_mmio_layout.base_region_id())
        .expect("first pmem MMIO region should be registered");
    assert_eq!(
        first_pmem_region.range().start(),
        pmem_mmio_layout.base_address()
    );
    assert_eq!(
        first_pmem_region.range().size(),
        bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE
    );
    let second_pmem_region_id =
        MmioRegionId::new(pmem_mmio_layout.base_region_id().raw_value() + 1);
    let second_pmem_region = mmio_regions
        .iter()
        .find(|region| region.id() == second_pmem_region_id)
        .expect("second pmem MMIO region should be registered");
    assert_eq!(second_pmem_region.id(), second_pmem_region_id);
    assert_eq!(
        second_pmem_region.range().start(),
        pmem_mmio_layout
            .base_address()
            .checked_add(pmem_mmio_layout.address_stride())
            .expect("second pmem MMIO address should fit")
    );
    assert_eq!(
        second_pmem_region.range().size(),
        bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE
    );
    let rtc_region = mmio_regions
        .iter()
        .find(|region| region.id() == rtc_mmio_layout.region_id())
        .expect("RTC MMIO region should be registered");
    assert_eq!(rtc_region.range().start(), rtc_mmio_layout.base());
    assert_eq!(
        rtc_region.range().size(),
        bangbang_runtime::rtc::RTC_MMIO_DEVICE_WINDOW_SIZE
    );
    assert!(session.block_interrupt_lines().is_empty());
    assert_eq!(session.pmem_interrupt_lines().len(), 2);
    assert_eq!(session.runtime_resources().pmem_devices.len(), 2);
    assert!(
        !session.runtime_resources().pmem_devices[0]
            .mapping()
            .is_read_only()
    );
    assert!(
        session.runtime_resources().pmem_devices[1]
            .mapping()
            .is_read_only()
    );
    assert!(
        !session.runtime_resources().pmem_devices[0]
            .guest_range()
            .overlaps(session.runtime_resources().layout.ranges()[0])
    );
    assert!(
        !session.runtime_resources().pmem_devices[0]
            .guest_range()
            .overlaps(session.runtime_resources().pmem_devices[1].guest_range())
    );
    assert_eq!(
        session
            .guest_memory()
            .expect("session should expose mapped guest memory")
            .total_size(),
        session.runtime_resources().layout.total_size()
    );
    let boot_origin = session
        .runtime_resources()
        .boot_origin
        .as_ref()
        .expect("ordinary session should retain boot-origin metadata");
    let boot_registers = session
        .boot_registers()
        .expect("ordinary session should retain boot registers");
    let mut fdt_magic = [0; 4];
    session
        .guest_memory()
        .expect("session should expose mapped guest memory")
        .read_slice(&mut fdt_magic, boot_origin.fdt.address)
        .expect("mapped guest memory should contain the written FDT");
    assert_eq!(u32::from_be_bytes(fdt_magic), 0xd00d_feed);
    assert_eq!(
        boot_registers.kernel_entry,
        boot_origin.loaded_boot_source.kernel.entry_address
    );
    assert_eq!(boot_registers.fdt_address, boot_origin.fdt.address);
    let register_state = session
        .capture_arm64_general_register_state()
        .expect("internal session should capture general-register state");
    assert_eq!(
        register_state.general_purpose_register(0),
        Some(boot_registers.fdt_address.raw_value())
    );
    assert_eq!(register_state.pc(), boot_registers.kernel_entry.raw_value());
    assert_eq!(register_state.cpsr(), ARM64_LINUX_BOOT_CPSR);
    session
        .restore_arm64_general_register_state(&register_state)
        .expect("internal session should restore general-register state");
    let core_system_register_state = session
        .capture_arm64_core_system_register_state()
        .expect("internal session should capture core system-register state");
    session
        .restore_arm64_core_system_register_state(&core_system_register_state)
        .expect("internal session should restore core system-register state");
    let exception_register_state = session
        .capture_arm64_exception_register_state()
        .expect("internal session should capture exception-register state");
    session
        .restore_arm64_exception_register_state(&exception_register_state)
        .expect("internal session should restore exception-register state");
    let execution_control_state = session
        .capture_arm64_execution_control_register_state()
        .expect("internal session should capture execution-control state");
    session
        .restore_arm64_execution_control_register_state(&execution_control_state)
        .expect("internal session should restore execution-control state");
    let cache_selection_state = session
        .capture_arm64_cache_selection_register_state()
        .expect("internal session should capture cache-selection state");
    session
        .restore_arm64_cache_selection_register_state(&cache_selection_state)
        .expect("internal session should restore cache-selection state");
    session
        .capture_arm64_breakpoint_register_state()
        .expect("internal session should capture breakpoint-register state");
    session
        .capture_arm64_watchpoint_register_state()
        .expect("internal session should capture watchpoint-register state");
    let debug_control_state = session
        .capture_arm64_debug_control_register_state()
        .expect("internal session should capture debug-control state");
    session
        .restore_arm64_debug_control_register_state(&debug_control_state)
        .expect("internal session should restore debug-control state");
    let debug_trap_state = session
        .capture_arm64_debug_trap_state()
        .expect("internal session should capture debug-trap state");
    session
        .restore_arm64_debug_trap_state(&debug_trap_state)
        .expect("internal session should restore debug-trap state");
    session
        .capture_arm64_identification_register_state()
        .expect("internal session should capture identification-register state");
    session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("internal session should capture SVE/SME identification state");
    let _sme_pstate =
        assert_sme_pstate_capture_supported_or_unavailable(session.capture_arm64_sme_pstate())
            .expect("internal session SME PSTATE capture should succeed or report unsupported");
    let _sme_p_registers = assert_sme_p_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_p_register_state(),
    )
    .expect("internal session SME P-register capture should succeed or report unavailable");
    let _sme_z_registers = assert_sme_z_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_z_register_state(),
    )
    .expect("internal session SME Z-register capture should succeed or report unavailable");
    let _sme_za_register = assert_sme_za_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_za_register_state(),
    )
    .expect("internal session SME ZA-register capture should succeed or report unavailable");
    let _sme_zt0_register = assert_sme_zt0_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_zt0_register_state(),
    )
    .expect("internal session SME ZT0-register capture should succeed or report unavailable");
    session
        .capture_arm64_sme_system_register_state()
        .expect("internal session should capture SME system-register state");
    let system_context_state = session
        .capture_arm64_system_context_register_state()
        .expect("internal session should capture system-context register state");
    session
        .restore_arm64_system_context_register_state(&system_context_state)
        .expect("internal session should restore system-context register state");
    let translation_state = session
        .capture_arm64_translation_register_state()
        .expect("internal session should capture translation-register state");
    session
        .restore_arm64_translation_register_state(&translation_state)
        .expect("internal session should restore translation-register state");
    let pointer_authentication_key_state = session
        .capture_arm64_pointer_authentication_key_state()
        .expect("internal session should capture pointer-authentication key state");
    session
        .restore_arm64_pointer_authentication_key_state(&pointer_authentication_key_state)
        .expect("internal session should restore pointer-authentication key state");
    let thread_context_state = session
        .capture_arm64_thread_context_register_state()
        .expect("internal session should capture thread-context register state");
    session
        .restore_arm64_thread_context_register_state(&thread_context_state)
        .expect("internal session should restore thread-context register state");
    let simd_fp_state = session
        .capture_arm64_simd_fp_state()
        .expect("internal session should capture SIMD/FP state");
    session
        .restore_arm64_simd_fp_state(&simd_fp_state)
        .expect("internal session should restore SIMD/FP state");
    session
        .capture_arm64_physical_timer_state()
        .expect("internal session should capture physical-timer state");
    session
        .capture_arm64_virtual_timer_state()
        .expect("internal session should capture virtual-timer state");
    let snapshot_timer_state = session
        .capture_arm64_snapshot_timer_state()
        .expect("internal session should capture normalized timer state");
    let pending_interrupt_state = session
        .capture_arm64_pending_interrupt_state()
        .expect("internal session should capture pending-interrupt state");
    session
        .restore_arm64_pending_interrupt_state(&pending_interrupt_state)
        .expect("internal session should restore pending-interrupt state");
    let gic_device_state = session
        .capture_gic_device_state()
        .expect("internal session should capture GIC device state");
    assert!(!gic_device_state.is_empty());
    let gic_icc_register_state = session
        .capture_arm64_gic_icc_register_state()
        .expect("internal session should capture GIC ICC register state");
    session
        .restore_gic_device_state(&gic_device_state)
        .expect("internal session should restore GIC device state before run");
    session
        .restore_arm64_gic_icc_register_state(&gic_icc_register_state)
        .expect("internal session should restore GIC ICC register state before run");
    let restored_gic_icc_register_state = session
        .capture_arm64_gic_icc_register_state()
        .expect("internal session should capture GIC ICC register state");
    assert!(
        restored_gic_icc_register_state == gic_icc_register_state,
        "internal session should preserve original GIC ICC register state"
    );
    session
        .restore_arm64_snapshot_timer_state(snapshot_timer_state)
        .expect("internal session should restore normalized timers after GIC state");
    assert_normalized_timer_restore_equivalent(
        snapshot_timer_state,
        session
            .capture_arm64_snapshot_timer_state()
            .expect("internal session should recapture normalized timers"),
    );
    let old_vmgenid = session.runtime_resources().vmgenid_device;
    session
        .replace_vmgenid_for_snapshot_restore()
        .expect("internal session should replace VMGenID and inject its SPI");
    let new_vmgenid = session.runtime_resources().vmgenid_device;
    assert_ne!(new_vmgenid.generation_id, old_vmgenid.generation_id);
    assert_eq!(new_vmgenid.range, old_vmgenid.range);
    assert_eq!(new_vmgenid.fdt_device, old_vmgenid.fdt_device);
    let mut guest_vmgenid = [0; bangbang_runtime::startup::ARM64_BOOT_VMGENID_SIZE];
    session
        .guest_memory()
        .expect("internal session should expose VMGenID guest memory")
        .read_slice(&mut guest_vmgenid, new_vmgenid.range.start())
        .expect("internal session replacement VMGenID should read");
    assert_eq!(guest_vmgenid, new_vmgenid.generation_id);
    let run_cancel_handle = session.run_cancel_handle();
    drop(run_cancel_handle);
    let run_loop_control = session.run_loop_control();
    let run_loop_stop_token = run_loop_control.stop_token();
    run_loop_control
        .request_stop()
        .expect("internal HVF boot-session run-loop stop should request vCPU cancellation");
    assert!(run_loop_stop_token.is_stop_requested());
    session
        .shutdown()
        .expect("internal HVF arm64 boot session should shut down");
    drop(session);

    let mut second_session = backend
        .prepare_arm64_boot_session(&controller, config)
        .expect("second internal HVF arm64 boot session should prepare after shutdown");
    assert_eq!(
        second_session
            .guest_memory_mut()
            .expect("second session should expose mutable mapped guest memory")
            .total_size(),
        second_session.runtime_resources().layout.total_size()
    );
    second_session
        .shutdown()
        .expect("second internal HVF arm64 boot session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_storage_traverses_signed_mmio_and_pci_owners() {
    use std::fs::OpenOptions;
    use std::os::unix::fs::FileExt;
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSnapshotV2CaptureInput, HvfArm64BootStorageCaptureErrorKind,
        HvfArm64BootStorageCaptureStage, HvfSnapshotV2BootState, HvfSnapshotV2DefaultProcessShell,
        HvfSnapshotV2NativePath, HvfSnapshotV2StorageMmioProcessConfig, HvfSnapshotV2StorageState,
        HvfVcpuRunStepOutcome, OwnedHvfArm64BootSession,
        prepare_hvf_snapshot_v2_storage_mmio_platform_plan,
        prepare_hvf_snapshot_v2_storage_pci_platform_plan,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{
        BlockCaptureIoEngine, BlockFileBacking, BlockMmioLayout, DriveCacheType, DriveConfigInput,
        DriveIoEngine, PreparedBlockDevice,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{
        PmemConfig, PmemConfigInput, PmemFileBacking, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT,
    };
    use bangbang_runtime::serial::{SharedSerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::snapshot_device_v2::{
        SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    };
    use bangbang_runtime::snapshot_device_v2_6::{
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2StorageDeviceGraph,
        SnapshotV2StorageRestorePlan,
    };
    use bangbang_runtime::storage_capture::{
        CaptureReadyStorageConfigs, StorageDeviceOrigin, StorageTransportState,
    };
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");

    let mmio_kernel = TempFile::new("capture-ready-mmio-kernel", &image)
        .expect("MMIO capture kernel should create");
    let mmio_root = TempFile::new_len("capture-ready-mmio-root", 4096)
        .expect("MMIO Sync backing should create");
    let mmio_async = TempFile::new_len("capture-ready-mmio-async", 4096)
        .expect("MMIO Async backing should create");
    let mmio_pmem = TempFile::new_len("capture-ready-mmio-pmem", VIRTIO_PMEM_ALIGNMENT)
        .expect("MMIO pmem backing should create");
    let mmio_read_only_pmem =
        TempFile::new_len("capture-ready-mmio-read-only-pmem", VIRTIO_PMEM_ALIGNMENT)
            .expect("read-only MMIO pmem backing should create");
    let mut mmio_controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    mmio_controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            mmio_kernel.path(),
        )))
        .expect("MMIO capture boot source should configure");
    mmio_controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", mmio_root.path(), true)
                .with_is_read_only(true)
                .with_io_engine(DriveIoEngine::Sync),
        ))
        .expect("MMIO Sync root should configure");
    mmio_controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("async", "async", mmio_async.path(), false)
                .with_is_read_only(false)
                .with_cache_type(DriveCacheType::Writeback)
                .with_io_engine(DriveIoEngine::Async),
        ))
        .expect("MMIO Async data drive should configure");
    mmio_controller
        .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
            "pmem0",
            path_text(mmio_pmem.path()),
        )))
        .expect("MMIO pmem should configure");
    mmio_controller
        .handle_action(VmmAction::PutPmem(
            PmemConfigInput::new("pmem_ro", path_text(mmio_read_only_pmem.path()))
                .with_read_only(true),
        ))
        .expect("read-only MMIO pmem should configure");
    let mmio_serial = SharedSerialOutputBuffer::default();
    let mmio_session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        bangbang_runtime::rtc::RtcMmioLayout::new(
            GuestAddress::new(0x4000_1000),
            MmioRegionId::new(10),
        ),
    )
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(20),
        GuestAddress::new(0x4000_2000),
        SharedSerialOutput::from(mmio_serial),
    ));
    let mut mmio_session = OwnedHvfArm64BootSession::new(&mmio_controller, mmio_session_config)
        .expect("signed MMIO storage session should prepare");
    let mmio_configs = CaptureReadyStorageConfigs::new(
        mmio_controller.drive_configs().to_vec(),
        mmio_controller.pmem_configs().to_vec(),
    );
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO retry publishers should quiesce");
    let profile_error = mmio_session
        .capture_snapshot_v2_multi_block_device_graph_at(&mmio_configs, &mmio_guard, Instant::now())
        .expect_err("pmem inventory must fail multi-block profile preflight");
    assert_eq!(
        profile_error.kind(),
        HvfArm64BootStorageCaptureErrorKind::ProfilePreflight
    );
    assert!(!profile_error.terminal());

    let mmio_pmem_mapping = mmio_session.runtime_resources().pmem_devices[0].mapping();
    // SAFETY: the retained prepared mapping is writable, and this one-byte
    // write stays inside its exact file-backed prefix.
    unsafe {
        mmio_pmem_mapping
            .host_address()
            .as_ptr()
            .cast::<u8>()
            .write(0x5a);
    }
    for cancelled_stage in [
        HvfArm64BootStorageCaptureStage::Inventory,
        HvfArm64BootStorageCaptureStage::StopAsync,
        HvfArm64BootStorageCaptureStage::DrainAsync,
        HvfArm64BootStorageCaptureStage::Persist,
        HvfArm64BootStorageCaptureStage::PublishAsync,
        HvfArm64BootStorageCaptureStage::Capture,
        HvfArm64BootStorageCaptureStage::ComposeGraph,
    ] {
        let error = mmio_session
            .capture_snapshot_v2_storage_device_graph_at_with_cancel(
                &mmio_configs,
                &mmio_guard,
                Instant::now(),
                |stage| stage == cancelled_stage,
            )
            .expect_err("profile-3 cancellation must not return a graph");
        assert_eq!(error.kind(), HvfArm64BootStorageCaptureErrorKind::Cancelled);
        assert!(!error.cleanup_failed());
        assert!(!error.terminal());
    }
    let mut persistence_visits = 0_u8;
    let between_devices_error = mmio_session
        .capture_snapshot_v2_storage_device_graph_at_with_cancel(
            &mmio_configs,
            &mmio_guard,
            Instant::now(),
            |stage| {
                if stage == HvfArm64BootStorageCaptureStage::Persist {
                    persistence_visits += 1;
                    persistence_visits == 4
                } else {
                    false
                }
            },
        )
        .expect_err("cancellation between pmem owners must not return a graph");
    assert_eq!(
        between_devices_error.kind(),
        HvfArm64BootStorageCaptureErrorKind::Cancelled
    );
    assert_eq!(
        persistence_visits, 4,
        "cancellation should occur after both block owners and the first pmem owner"
    );
    assert!(!between_devices_error.cleanup_failed());
    assert!(!between_devices_error.terminal());
    let mut mmio_stages = Vec::new();
    let mmio_graph = mmio_session
        .capture_snapshot_v2_storage_device_graph_at_with_cancel(
            &mmio_configs,
            &mmio_guard,
            Instant::now(),
            |stage| {
                mmio_stages.push(stage);
                false
            },
        )
        .expect("signed MMIO profile-3 storage graph should capture");
    let last_drain = mmio_stages
        .iter()
        .rposition(|stage| *stage == HvfArm64BootStorageCaptureStage::DrainAsync)
        .expect("profile-3 capture should observe Async drain");
    let first_persist = mmio_stages
        .iter()
        .position(|stage| *stage == HvfArm64BootStorageCaptureStage::Persist)
        .expect("profile-3 capture should observe persistence");
    let first_publication = mmio_stages
        .iter()
        .position(|stage| *stage == HvfArm64BootStorageCaptureStage::PublishAsync)
        .expect("profile-3 capture should observe completion publication");
    let first_capture = mmio_stages
        .iter()
        .position(|stage| *stage == HvfArm64BootStorageCaptureStage::Capture)
        .expect("profile-3 capture should observe live capture");
    let first_composition = mmio_stages
        .iter()
        .position(|stage| *stage == HvfArm64BootStorageCaptureStage::ComposeGraph)
        .expect("profile-3 capture should observe graph composition");
    assert!(last_drain < first_persist);
    assert!(first_persist < first_publication);
    assert!(first_publication < first_capture);
    assert!(first_capture < first_composition);
    assert_eq!(
        std::fs::read(mmio_pmem.path())
            .expect("persisted MMIO pmem backing should read")
            .first()
            .copied(),
        Some(0x5a)
    );
    assert_eq!(
        mmio_graph.transport_kind(),
        SnapshotV2DeviceTransportKind::Mmio
    );
    assert_eq!(mmio_graph.block_records().len(), 2);
    assert_eq!(mmio_graph.pmem_records().len(), 2);
    assert_eq!(
        mmio_graph.root_key(),
        Some(mmio_graph.block_records()[0].key())
    );
    assert_eq!(mmio_graph.pmem_records()[0].config().pmem_id(), "pmem0");
    assert_eq!(
        mmio_graph.pmem_records()[0].pmem().file_bytes(),
        VIRTIO_PMEM_ALIGNMENT
    );
    assert!(mmio_graph.pmem_records()[1].config().is_read_only());
    assert_eq!(
        std::fs::read(mmio_read_only_pmem.path())
            .expect("read-only MMIO pmem backing should read")
            .first()
            .copied(),
        Some(0)
    );
    assert!(matches!(
        mmio_graph.pmem_records()[0].transport(),
        SnapshotV2DeviceTransport::Mmio(_)
    ));
    let mmio_graph_bytes = mmio_graph
        .encode(NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION)
        .expect("captured MMIO profile-3 graph should encode");
    assert_eq!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &mmio_graph_bytes,
        )
        .expect("captured MMIO profile-3 graph should decode"),
        mmio_graph
    );
    assert_eq!(
        mmio_session
            .capture_snapshot_v2_storage_device_graph_at(
                &mmio_configs,
                &mmio_guard,
                Instant::now(),
            )
            .expect("MMIO profile-3 capture should repeat"),
        mmio_graph
    );
    mmio_session
        .pause_for_snapshot_v2_capture()
        .expect("MMIO profile-3 source should pause");
    let mmio_boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_new(mmio_kernel.path().as_os_str())
            .expect("MMIO profile-3 kernel path should validate"),
        None,
        None,
    )
    .expect("MMIO profile-3 boot metadata should validate");
    let mmio_memory = TempFile::new_len("capture-ready-mmio-profile-3-memory", 0)
        .expect("MMIO profile-3 memory artifact should create");
    let mut mmio_memory_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(mmio_memory.path())
        .expect("MMIO profile-3 memory artifact should open");
    let mmio_platform = mmio_session
        .capture_snapshot_v2_storage_platform_with_cancel(
            HvfArm64BootSnapshotV2CaptureInput::new(mmio_boot),
            &mut mmio_memory_writer,
            |_| false,
        )
        .expect("MMIO exact 2.6 platform should capture");
    assert_eq!(
        mmio_platform.memory().version(),
        NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION
    );
    assert_eq!(
        mmio_memory_writer
            .metadata()
            .expect("MMIO profile-3 memory metadata should read")
            .len(),
        mmio_platform.memory().file_length()
    );
    let mmio_complete = HvfSnapshotV2StorageState::try_new(mmio_platform, mmio_graph.clone())
        .expect("MMIO exact 2.6 platform and storage graph should compose");
    assert_eq!(mmio_complete.device_graph(), &mmio_graph);
    drop(mmio_memory_writer);
    mmio_session
        .resume_after_snapshot_v2_capture()
        .expect("MMIO profile-3 source should resume after capture");

    let mmio_first = mmio_session
        .capture_ready_storage_state_at(&mmio_configs, &mmio_guard, Instant::now())
        .expect("signed MMIO storage should become capture-ready");
    let mmio_second = mmio_session
        .capture_ready_storage_state_at(&mmio_configs, &mmio_guard, Instant::now())
        .expect("MMIO Async admission should reopen for a second capture");

    assert_eq!(mmio_first.block_devices().len(), 2);
    assert_eq!(mmio_first.pmem_devices().len(), 2);
    for (captured, configured) in mmio_first
        .block_devices()
        .iter()
        .zip(mmio_controller.drive_configs())
    {
        assert_eq!(captured.config(), configured);
        assert!(matches!(
            captured.transport(),
            StorageTransportState::Mmio(_)
        ));
    }
    assert_eq!(
        mmio_first.block_devices()[0].device().io_engine(),
        BlockCaptureIoEngine::Sync
    );
    let BlockCaptureIoEngine::Async(mmio_async_first) =
        mmio_first.block_devices()[1].device().io_engine()
    else {
        panic!("second MMIO drive should retain Async continuation state");
    };
    let BlockCaptureIoEngine::Async(mmio_async_second) =
        mmio_second.block_devices()[1].device().io_engine()
    else {
        panic!("second MMIO capture should retain Async continuation state");
    };
    assert_eq!(
        mmio_async_second.generation(),
        mmio_async_first.generation()
    );
    assert!(mmio_async_first.admission_stopped());
    assert_eq!(mmio_async_first.owned_operations(), 0);
    assert_eq!(mmio_async_first.parked_host_completions(), 0);
    assert_eq!(mmio_async_first.final_completions(), 0);
    assert_eq!(
        mmio_first.pmem_devices()[0].config(),
        &mmio_controller.pmem_configs()[0]
    );
    assert!(matches!(
        mmio_first.pmem_devices()[0].transport(),
        StorageTransportState::Mmio(_)
    ));
    assert!(
        mmio_first.pmem_devices()[0]
            .mapping()
            .same_mapping(mmio_second.pmem_devices()[0].mapping())
    );
    let mmio_debug = format!(
        "{:?} {:?}",
        mmio_first.block_devices(),
        mmio_first.pmem_devices()
    );
    for private_path in [
        path_text(mmio_root.path()),
        path_text(mmio_async.path()),
        path_text(mmio_pmem.path()),
        path_text(mmio_read_only_pmem.path()),
    ] {
        assert!(!mmio_debug.contains(&private_path));
    }

    let restored_layout =
        GuestMemoryLayout::new(mmio_session.runtime_resources().layout.ranges().to_vec())
            .expect("MMIO profile-3 destination layout should validate");
    let mut fault_memory = GuestMemory::allocate(&restored_layout)
        .expect("MMIO profile-3 fault destination memory should allocate");
    let mut restored_memory = GuestMemory::allocate(&restored_layout)
        .expect("MMIO profile-3 destination memory should allocate");
    let source_memory = mmio_session
        .guest_memory()
        .expect("MMIO profile-3 source memory should remain mapped");
    let mut copy_buffer = vec![0_u8; 64 * 1024];
    for range in restored_layout.ranges() {
        let mut copied = 0_u64;
        while copied < range.size() {
            let remaining = range.size() - copied;
            let count =
                usize::try_from(remaining.min(
                    u64::try_from(copy_buffer.len()).expect("copy buffer length should fit u64"),
                ))
                .expect("MMIO profile-3 copy count should fit usize");
            let address = range
                .start()
                .checked_add(copied)
                .expect("MMIO profile-3 copy address should fit");
            source_memory
                .read_slice(&mut copy_buffer[..count], address)
                .expect("MMIO profile-3 source memory should read");
            fault_memory
                .write_slice(&copy_buffer[..count], address)
                .expect("MMIO profile-3 fault destination memory should write");
            restored_memory
                .write_slice(&copy_buffer[..count], address)
                .expect("MMIO profile-3 destination memory should write");
            copied += u64::try_from(count).expect("copy count should fit u64");
        }
    }
    drop(mmio_guard);
    mmio_session
        .shutdown()
        .expect("signed MMIO storage session should shut down");

    let (source_platform, source_graph) = mmio_complete.into_parts();
    let reopen_block_backings = || {
        source_graph
            .block_records()
            .iter()
            .map(|record| {
                BlockFileBacking::open_snapshot(
                    std::path::Path::new(record.config().selector()),
                    record.config().is_read_only(),
                )
                .map(|(backing, _identity)| backing)
                .expect("MMIO profile-3 block backing should reopen")
            })
            .collect::<Vec<_>>()
    };
    let reopen_pmem_backings = || {
        mmio_controller
            .pmem_configs()
            .iter()
            .map(|config| {
                PmemFileBacking::open(config).expect("MMIO profile-3 pmem backing should reopen")
            })
            .collect::<Vec<_>>()
    };
    let restore_process_config = HvfSnapshotV2StorageMmioProcessConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
    );
    let restore_now = Instant::now();

    let fault_bundle =
        SnapshotV2StorageRestorePlan::prepare(source_graph.clone(), &fault_memory, restore_now)
            .expect("MMIO profile-3 fault restore plan should validate")
            .prepare_backings(reopen_block_backings(), reopen_pmem_backings(), || false)
            .expect("MMIO profile-3 fault backings should prepare");
    let fault_plan = prepare_hvf_snapshot_v2_storage_mmio_platform_plan(
        &source_platform,
        &fault_bundle,
        restore_process_config,
    )
    .expect("MMIO profile-3 fault platform plan should validate");
    let fault_shell = HvfSnapshotV2DefaultProcessShell::new(SharedSerialOutput::from(
        SharedSerialOutputBuffer::default(),
    ));
    let fault =
        OwnedHvfArm64BootSession::restore_snapshot_v2_storage_mmio_with_pmem_publication_fault(
            source_platform.clone(),
            fault_memory,
            fault_shell,
            fault_bundle,
            fault_plan,
            0,
        )
        .expect_err("injected post-mapping publication fault should reject");
    assert_eq!(
        fault.stage(),
        bangbang_hvf::HvfSnapshotV2StorageMmioRestoreStage::Registration {
            index: source_graph.block_records().len(),
        }
    );
    assert!(fault.cleanup_failures().is_empty());
    assert!(!fault.is_terminal());
    let fault_diagnostics = format!("{fault:?} {fault}");
    for private_path in [
        path_text(mmio_root.path()),
        path_text(mmio_async.path()),
        path_text(mmio_pmem.path()),
        path_text(mmio_read_only_pmem.path()),
    ] {
        assert!(!fault_diagnostics.contains(&private_path));
    }

    let restore_bundle =
        SnapshotV2StorageRestorePlan::prepare(source_graph.clone(), &restored_memory, restore_now)
            .expect("MMIO profile-3 restore plan should validate")
            .prepare_backings(reopen_block_backings(), reopen_pmem_backings(), || false)
            .expect("MMIO profile-3 backings should prepare");
    let restore_plan = prepare_hvf_snapshot_v2_storage_mmio_platform_plan(
        &source_platform,
        &restore_bundle,
        restore_process_config,
    )
    .expect("MMIO profile-3 platform plan should validate");
    let restored_serial = SharedSerialOutputBuffer::default();
    let restore_shell =
        HvfSnapshotV2DefaultProcessShell::new(SharedSerialOutput::from(restored_serial));
    let restored_owners = OwnedHvfArm64BootSession::restore_snapshot_v2_storage_mmio(
        source_platform.clone(),
        restored_memory,
        restore_shell,
        restore_bundle,
        restore_plan,
    )
    .unwrap_or_else(|error| panic!("MMIO profile-3 owners should restore: {error:?}"));
    assert_eq!(restored_owners.configs(), &mmio_configs);
    let (mut restored, restored_configs) = restored_owners.into_parts();
    assert_eq!(restored_configs, mmio_configs);
    assert_eq!(restored.runtime_resources().block_devices.len(), 2);
    assert_eq!(restored.runtime_resources().pmem_devices.len(), 2);
    assert_eq!(restored.runtime_resources().pmem_mmio_devices.len(), 2);
    assert!(restored.runtime_resources().pci_block_devices.is_empty());
    assert!(!restored.uses_pci_data_devices());

    for ((device, registration), record) in restored
        .runtime_resources()
        .pmem_devices
        .iter()
        .zip(&restored.runtime_resources().pmem_mmio_devices)
        .zip(source_graph.pmem_records())
    {
        let SnapshotV2DeviceTransport::Mmio(transport) = record.transport() else {
            panic!("restored profile-3 pmem record should use MMIO");
        };
        assert_eq!(device.id(), record.config().pmem_id());
        assert_eq!(device.guest_range(), record.pmem().guest_range());
        assert_eq!(device.config_space(), record.pmem().config_space());
        assert_eq!(registration.registration.region(), transport.region());
        assert_eq!(
            registration.fdt_device.interrupt_line,
            transport.interrupt_line()
        );
    }
    let block_metrics = restored.shared_block_device_metrics();
    for config in restored_configs.drives() {
        assert!(
            block_metrics
                .per_drive(config.drive_id())
                .expect("restored block metrics owner should exist")
                .snapshot()
                .is_empty()
        );
    }
    let pmem_metrics = restored.shared_pmem_device_metrics();
    for config in restored_configs.pmem() {
        assert!(
            pmem_metrics
                .per_device(config.id())
                .expect("restored pmem metrics owner should exist")
                .snapshot()
                .is_empty()
        );
    }

    let restored_guard = restored
        .quiesce_limiter_retry_wakeups()
        .expect("restored profile-3 retry publishers should quiesce");
    let recaptured_graph = restored
        .capture_snapshot_v2_storage_device_graph_at(
            &restored_configs,
            &restored_guard,
            restore_now,
        )
        .expect("restored MMIO profile-3 graph should recapture");
    assert_eq!(recaptured_graph, source_graph);
    let recaptured_memory = TempFile::new_len("restored-mmio-profile-3-memory", 0)
        .expect("restored MMIO profile-3 memory artifact should create");
    let mut recaptured_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(recaptured_memory.path())
        .expect("restored MMIO profile-3 memory artifact should open");
    let recaptured_boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_new(mmio_kernel.path().as_os_str())
            .expect("restored MMIO profile-3 kernel path should validate"),
        None,
        None,
    )
    .expect("restored MMIO profile-3 boot metadata should validate");
    let recaptured_platform = restored
        .capture_snapshot_v2_storage_platform_with_cancel(
            HvfArm64BootSnapshotV2CaptureInput::new(recaptured_boot),
            &mut recaptured_writer,
            |_| false,
        )
        .expect("restored MMIO profile-3 platform should recapture");
    assert_native_v2_platform_recapture_equivalent(&source_platform, &recaptured_platform);
    drop(restored_guard);

    const RESTORED_PMEM_WRITE_OFFSET: u64 = 4096;
    const RESTORED_PMEM_WRITE_VALUE: u32 = 0x1631_cafe;
    let restored_entry = GuestAddress::new(
        restored
            .capture_arm64_general_register_state()
            .expect("restored MMIO profile-3 registers should capture")
            .pc(),
    );
    let restored_target = restored.runtime_resources().pmem_devices[0]
        .guest_range()
        .start()
        .checked_add(RESTORED_PMEM_WRITE_OFFSET)
        .expect("restored MMIO profile-3 pmem target should fit");
    let restored_program =
        arm64_store_u32_and_hvc_program(restored_target.raw_value(), RESTORED_PMEM_WRITE_VALUE);
    restored
        .guest_memory_mut()
        .expect("restored MMIO profile-3 guest memory should map")
        .write_slice(&restored_program, restored_entry)
        .expect("restored MMIO profile-3 guest program should write");
    restored
        .resume_after_snapshot_v2_capture()
        .expect("restored MMIO profile-3 runner should resume");
    assert!(matches!(
        restored
            .run_once_and_handle_mmio()
            .expect("restored guest should reach HVC through the pmem mapping"),
        HvfVcpuRunStepOutcome::Hvc { exit, .. } if exit.immediate() == 0
    ));
    let observer =
        std::fs::File::open(mmio_pmem.path()).expect("restored pmem observer should open");
    let mut observed = [0_u8; std::mem::size_of::<u32>()];
    observer
        .read_exact_at(&mut observed, RESTORED_PMEM_WRITE_OFFSET)
        .expect("restored pmem observer should read");
    assert_eq!(u32::from_le_bytes(observed), RESTORED_PMEM_WRITE_VALUE);
    restored
        .shutdown()
        .expect("restored MMIO profile-3 session should tear down in ownership order");

    let pci_kernel = TempFile::new("capture-ready-pci-kernel", &image)
        .expect("PCI capture kernel should create");
    let pci_root = TempFile::new_len("capture-ready-pci-root", 4096)
        .expect("startup PCI Sync backing should create");
    let pci_startup_pmem =
        TempFile::new_len("capture-ready-pci-startup-pmem", VIRTIO_PMEM_ALIGNMENT)
            .expect("startup PCI pmem backing should create");
    let pci_dynamic_async = TempFile::new_len("capture-ready-pci-dynamic-async", 4096)
        .expect("runtime PCI Async backing should create");
    let pci_dynamic_pmem =
        TempFile::new_len("capture-ready-pci-dynamic-pmem", VIRTIO_PMEM_ALIGNMENT)
            .expect("runtime PCI pmem backing should create");
    let mut pci_controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    pci_controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            pci_kernel.path(),
        )))
        .expect("PCI capture boot source should configure");
    pci_controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", pci_root.path(), true)
                .with_is_read_only(true)
                .with_io_engine(DriveIoEngine::Sync),
        ))
        .expect("startup PCI Sync root should configure");
    pci_controller
        .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
            "startup_pmem",
            path_text(pci_startup_pmem.path()),
        )))
        .expect("startup PCI pmem should configure");
    let pci_serial = SharedSerialOutputBuffer::default();
    let pci_session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        bangbang_runtime::rtc::RtcMmioLayout::new(
            GuestAddress::new(0x4000_1000),
            MmioRegionId::new(10),
        ),
    )
    .with_pci_enabled()
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(20),
        GuestAddress::new(0x4000_2000),
        SharedSerialOutput::from(pci_serial),
    ));
    let mut pci_session = OwnedHvfArm64BootSession::new(&pci_controller, pci_session_config)
        .expect("signed startup PCI storage session should prepare");

    let dynamic_drive_input =
        DriveConfigInput::new("hotdata", "hotdata", pci_dynamic_async.path(), false)
            .with_is_read_only(false)
            .with_cache_type(DriveCacheType::Writeback)
            .with_io_engine(DriveIoEngine::Async);
    let dynamic_drive = dynamic_drive_input
        .clone()
        .validate()
        .expect("runtime PCI Async config should validate");
    pci_controller
        .handle_action(VmmAction::PutDrive(dynamic_drive_input))
        .expect("runtime PCI Async config should join current inventory");
    pci_session
        .insert_runtime_block_device(
            PreparedBlockDevice::from_config_with_backing(&dynamic_drive, None)
                .expect("runtime PCI Async device should prepare"),
        )
        .expect("runtime PCI Async device should publish");

    let dynamic_pmem_input = PmemConfigInput::new("hotpmem", path_text(pci_dynamic_pmem.path()));
    let dynamic_pmem = PmemConfig::try_from(dynamic_pmem_input.clone())
        .expect("runtime PCI pmem config should validate");
    pci_controller
        .handle_action(VmmAction::PutPmem(dynamic_pmem_input))
        .expect("runtime PCI pmem config should join current inventory");
    pci_session
        .insert_runtime_pmem_device(
            &dynamic_pmem,
            PmemFileBacking::open(&dynamic_pmem).expect("runtime PCI pmem backing should open"),
        )
        .expect("runtime PCI pmem device should publish");

    let pci_configs = CaptureReadyStorageConfigs::new(
        pci_controller.drive_configs().to_vec(),
        pci_controller.pmem_configs().to_vec(),
    );
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI retry publishers should quiesce");
    for (index, byte) in [0x61_u8, 0x62].into_iter().enumerate() {
        let mapping = pci_session.runtime_resources().pmem_devices[index].mapping();
        // SAFETY: both retained prepared mappings are writable, and each
        // one-byte write stays inside its exact file-backed prefix.
        unsafe {
            mapping.host_address().as_ptr().cast::<u8>().write(byte);
        }
    }
    let pci_graph = pci_session
        .capture_snapshot_v2_storage_device_graph_at(&pci_configs, &pci_guard, Instant::now())
        .expect("signed startup/runtime PCI profile-3 graph should capture");
    assert_eq!(
        pci_graph.transport_kind(),
        SnapshotV2DeviceTransportKind::Pci
    );
    assert_eq!(pci_graph.block_records().len(), 2);
    assert_eq!(pci_graph.pmem_records().len(), 2);
    assert_eq!(
        pci_graph.root_key(),
        Some(pci_graph.block_records()[0].key())
    );
    assert_eq!(
        pci_graph
            .pmem_records()
            .iter()
            .map(|record| match record.transport() {
                SnapshotV2DeviceTransport::Pci(state) => state.origin(),
                SnapshotV2DeviceTransport::Mmio(_) => {
                    panic!("PCI pmem graph record must not use MMIO")
                }
            })
            .collect::<Vec<_>>(),
        vec![StorageDeviceOrigin::Startup, StorageDeviceOrigin::Runtime]
    );
    assert!(!pci_graph.pmem_records()[1].config().is_root());
    assert_eq!(
        std::fs::read(pci_startup_pmem.path())
            .expect("persisted startup PCI pmem should read")
            .first()
            .copied(),
        Some(0x61)
    );
    assert_eq!(
        std::fs::read(pci_dynamic_pmem.path())
            .expect("persisted runtime PCI pmem should read")
            .first()
            .copied(),
        Some(0x62)
    );
    let pci_graph_bytes = pci_graph
        .encode(NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION)
        .expect("captured PCI profile-3 graph should encode");
    assert_eq!(
        SnapshotV2StorageDeviceGraph::decode(
            NATIVE_V2_STORAGE_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &pci_graph_bytes,
        )
        .expect("captured PCI profile-3 graph should decode"),
        pci_graph
    );
    assert_eq!(
        pci_session
            .capture_snapshot_v2_storage_device_graph_at(&pci_configs, &pci_guard, Instant::now(),)
            .expect("PCI profile-3 capture should repeat"),
        pci_graph
    );

    let pci_first = pci_session
        .capture_ready_storage_state_at(&pci_configs, &pci_guard, Instant::now())
        .expect("signed startup/runtime PCI storage should become capture-ready");
    let pci_second = pci_session
        .capture_ready_storage_state_at(&pci_configs, &pci_guard, Instant::now())
        .expect("runtime PCI Async admission should reopen for a second capture");

    assert_eq!(pci_first.block_devices().len(), 2);
    assert_eq!(pci_first.pmem_devices().len(), 2);
    for (captured, configured) in pci_first
        .block_devices()
        .iter()
        .zip(pci_controller.drive_configs())
    {
        assert_eq!(captured.config(), configured);
    }
    for (captured, configured) in pci_first
        .pmem_devices()
        .iter()
        .zip(pci_controller.pmem_configs())
    {
        assert_eq!(captured.config(), configured);
    }
    let block_origins = pci_first
        .block_devices()
        .iter()
        .map(|device| match device.transport() {
            StorageTransportState::Pci(transport) => transport.origin(),
            StorageTransportState::Mmio(_) => panic!("PCI block should not capture as MMIO"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        block_origins,
        vec![StorageDeviceOrigin::Startup, StorageDeviceOrigin::Runtime]
    );
    let pmem_origins = pci_first
        .pmem_devices()
        .iter()
        .map(|device| match device.transport() {
            StorageTransportState::Pci(transport) => transport.origin(),
            StorageTransportState::Mmio(_) => panic!("PCI pmem should not capture as MMIO"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        pmem_origins,
        vec![StorageDeviceOrigin::Startup, StorageDeviceOrigin::Runtime]
    );
    let BlockCaptureIoEngine::Async(pci_async_first) =
        pci_first.block_devices()[1].device().io_engine()
    else {
        panic!("runtime PCI drive should retain Async continuation state");
    };
    let BlockCaptureIoEngine::Async(pci_async_second) =
        pci_second.block_devices()[1].device().io_engine()
    else {
        panic!("second runtime PCI capture should retain Async continuation state");
    };
    assert_eq!(pci_async_second.generation(), pci_async_first.generation());
    assert!(pci_async_first.admission_stopped());
    assert_eq!(pci_async_first.owned_operations(), 0);
    assert_eq!(pci_async_first.parked_host_completions(), 0);
    assert_eq!(pci_async_first.final_completions(), 0);
    for (first, second) in pci_first
        .pmem_devices()
        .iter()
        .zip(pci_second.pmem_devices())
    {
        assert!(first.mapping().same_mapping(second.mapping()));
    }
    let pci_debug = format!(
        "{:?} {:?}",
        pci_first.block_devices(),
        pci_first.pmem_devices()
    );
    for private_path in [
        path_text(pci_root.path()),
        path_text(pci_startup_pmem.path()),
        path_text(pci_dynamic_async.path()),
        path_text(pci_dynamic_pmem.path()),
    ] {
        assert!(!pci_debug.contains(&private_path));
    }

    let pci_restored_layout =
        GuestMemoryLayout::new(pci_session.runtime_resources().layout.ranges().to_vec())
            .expect("PCI profile-3 destination layout should validate");
    let mut pci_fault_memory = GuestMemory::allocate(&pci_restored_layout)
        .expect("PCI profile-3 fault destination memory should allocate");
    let mut pci_restored_memory = GuestMemory::allocate(&pci_restored_layout)
        .expect("PCI profile-3 destination memory should allocate");
    let pci_source_memory = pci_session
        .guest_memory()
        .expect("PCI profile-3 source memory should remain mapped");
    let mut pci_copy_buffer = vec![0_u8; 64 * 1024];
    for range in pci_restored_layout.ranges() {
        let mut copied = 0_u64;
        while copied < range.size() {
            let count = usize::try_from(
                (range.size() - copied).min(
                    u64::try_from(pci_copy_buffer.len())
                        .expect("PCI copy buffer length should fit u64"),
                ),
            )
            .expect("PCI profile-3 copy count should fit usize");
            let address = range
                .start()
                .checked_add(copied)
                .expect("PCI profile-3 copy address should fit");
            pci_source_memory
                .read_slice(&mut pci_copy_buffer[..count], address)
                .expect("PCI profile-3 source memory should read");
            pci_fault_memory
                .write_slice(&pci_copy_buffer[..count], address)
                .expect("PCI profile-3 fault destination memory should write");
            pci_restored_memory
                .write_slice(&pci_copy_buffer[..count], address)
                .expect("PCI profile-3 destination memory should write");
            copied += u64::try_from(count).expect("PCI copy count should fit u64");
        }
    }
    pci_session
        .pause_for_snapshot_v2_capture()
        .expect("PCI profile-3 source should pause");
    let pci_boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_new(pci_kernel.path().as_os_str())
            .expect("PCI profile-3 kernel path should validate"),
        None,
        None,
    )
    .expect("PCI profile-3 boot metadata should validate");
    let pci_memory = TempFile::new_len("capture-ready-pci-profile-3-memory", 0)
        .expect("PCI profile-3 memory artifact should create");
    let mut pci_memory_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(pci_memory.path())
        .expect("PCI profile-3 memory artifact should open");
    let pci_platform = pci_session
        .capture_snapshot_v2_storage_platform_with_cancel(
            HvfArm64BootSnapshotV2CaptureInput::new(pci_boot),
            &mut pci_memory_writer,
            |_| false,
        )
        .expect("PCI exact 2.6 platform should capture");
    assert_eq!(
        pci_memory_writer
            .metadata()
            .expect("PCI profile-3 memory metadata should read")
            .len(),
        pci_platform.memory().file_length()
    );
    drop(pci_memory_writer);
    drop(pci_guard);
    pci_session
        .shutdown()
        .expect("signed PCI storage session should shut down");

    let reopen_pci_block_backings = || {
        pci_graph
            .block_records()
            .iter()
            .map(|record| {
                BlockFileBacking::open_snapshot(
                    std::path::Path::new(record.config().selector()),
                    record.config().is_read_only(),
                )
                .map(|(backing, _identity)| backing)
                .expect("PCI profile-3 block backing should reopen")
            })
            .collect::<Vec<_>>()
    };
    let reopen_pci_pmem_backings = || {
        pci_controller
            .pmem_configs()
            .iter()
            .map(|config| {
                PmemFileBacking::open(config).expect("PCI profile-3 pmem backing should reopen")
            })
            .collect::<Vec<_>>()
    };
    let pci_restore_now = Instant::now();
    let pci_fault_bundle = SnapshotV2StorageRestorePlan::prepare(
        pci_graph.clone(),
        &pci_fault_memory,
        pci_restore_now,
    )
    .expect("PCI profile-3 fault restore plan should validate")
    .prepare_backings(
        reopen_pci_block_backings(),
        reopen_pci_pmem_backings(),
        || false,
    )
    .expect("PCI profile-3 fault backings should prepare");
    let pci_fault_plan =
        prepare_hvf_snapshot_v2_storage_pci_platform_plan(&pci_platform, &pci_fault_bundle)
            .expect("PCI profile-3 fault platform plan should validate");
    let pci_fault_shell = HvfSnapshotV2DefaultProcessShell::new(SharedSerialOutput::from(
        SharedSerialOutputBuffer::default(),
    ));
    let pci_fault =
        OwnedHvfArm64BootSession::restore_snapshot_v2_storage_pci_with_pmem_publication_fault(
            pci_platform.clone(),
            pci_fault_memory,
            pci_fault_shell,
            pci_fault_bundle,
            pci_fault_plan,
            0,
        )
        .expect_err("injected PCI post-mapping publication fault should reject");
    assert_eq!(
        pci_fault.stage(),
        bangbang_hvf::HvfSnapshotV2StoragePciRestoreStage::Publication {
            index: pci_graph.block_records().len(),
        }
    );
    assert!(
        pci_fault.cleanup_failures().is_empty(),
        "PCI publication rollback should be complete: {:?}",
        pci_fault.cleanup_failures()
    );
    assert!(!pci_fault.is_terminal());
    let pci_fault_diagnostics = format!("{pci_fault:?} {pci_fault}");
    for private_path in [
        path_text(pci_root.path()),
        path_text(pci_startup_pmem.path()),
        path_text(pci_dynamic_async.path()),
        path_text(pci_dynamic_pmem.path()),
    ] {
        assert!(!pci_fault_diagnostics.contains(&private_path));
    }

    let pci_restore_bundle = SnapshotV2StorageRestorePlan::prepare(
        pci_graph.clone(),
        &pci_restored_memory,
        pci_restore_now,
    )
    .expect("PCI profile-3 restore plan should validate")
    .prepare_backings(
        reopen_pci_block_backings(),
        reopen_pci_pmem_backings(),
        || false,
    )
    .expect("PCI profile-3 backings should prepare");
    let pci_restore_plan =
        prepare_hvf_snapshot_v2_storage_pci_platform_plan(&pci_platform, &pci_restore_bundle)
            .expect("PCI profile-3 platform plan should validate");
    let pci_restore_shell = HvfSnapshotV2DefaultProcessShell::new(SharedSerialOutput::from(
        SharedSerialOutputBuffer::default(),
    ));
    let pci_restored_owners = OwnedHvfArm64BootSession::restore_snapshot_v2_storage_pci(
        pci_platform,
        pci_restored_memory,
        pci_restore_shell,
        pci_restore_bundle,
        pci_restore_plan,
    )
    .unwrap_or_else(|error| panic!("PCI profile-3 owners should restore: {error:?}"));
    assert_eq!(pci_restored_owners.configs(), &pci_configs);
    let (mut pci_restored, pci_restored_configs) = pci_restored_owners.into_parts();
    assert_eq!(pci_restored_configs, pci_configs);
    assert!(pci_restored.uses_pci_data_devices());
    assert!(pci_restored.runtime_resources().block_devices.is_empty());
    assert_eq!(pci_restored.runtime_resources().pmem_devices.len(), 2);
    assert!(
        pci_restored
            .runtime_resources()
            .pmem_mmio_devices
            .is_empty()
    );
    let pci_restored_guard = pci_restored
        .quiesce_limiter_retry_wakeups()
        .expect("restored PCI profile-3 retry publishers should quiesce");
    let pci_recaptured_graph = pci_restored
        .capture_snapshot_v2_storage_device_graph_at(
            &pci_restored_configs,
            &pci_restored_guard,
            pci_restore_now,
        )
        .expect("restored PCI profile-3 graph should recapture");
    assert_eq!(pci_recaptured_graph, pci_graph);
    drop(pci_restored_guard);

    const RESTORED_PCI_PMEM_WRITE_OFFSET: u64 = 8192;
    const RESTORED_PCI_PMEM_WRITE_VALUE: u32 = 0x1632_cafe;
    let pci_restored_entry = GuestAddress::new(
        pci_restored
            .capture_arm64_general_register_state()
            .expect("restored PCI profile-3 registers should capture")
            .pc(),
    );
    let pci_restored_target = pci_restored.runtime_resources().pmem_devices[0]
        .guest_range()
        .start()
        .checked_add(RESTORED_PCI_PMEM_WRITE_OFFSET)
        .expect("restored PCI profile-3 pmem target should fit");
    let pci_restored_program = arm64_store_u32_and_hvc_program(
        pci_restored_target.raw_value(),
        RESTORED_PCI_PMEM_WRITE_VALUE,
    );
    pci_restored
        .guest_memory_mut()
        .expect("restored PCI profile-3 guest memory should map")
        .write_slice(&pci_restored_program, pci_restored_entry)
        .expect("restored PCI profile-3 guest program should write");
    pci_restored
        .resume_after_snapshot_v2_capture()
        .expect("restored PCI profile-3 runner should resume");
    assert!(matches!(
        pci_restored
            .run_once_and_handle_mmio()
            .expect("restored PCI guest should reach HVC through its pmem mapping"),
        HvfVcpuRunStepOutcome::Hvc { exit, .. } if exit.immediate() == 0
    ));
    let pci_observer =
        std::fs::File::open(pci_startup_pmem.path()).expect("restored PCI pmem should open");
    let mut pci_observed = [0_u8; std::mem::size_of::<u32>()];
    pci_observer
        .read_exact_at(&mut pci_observed, RESTORED_PCI_PMEM_WRITE_OFFSET)
        .expect("restored PCI pmem observer should read");
    assert_eq!(
        u32::from_le_bytes(pci_observed),
        RESTORED_PCI_PMEM_WRITE_VALUE
    );
    pci_restored
        .shutdown()
        .expect("restored PCI profile-3 session should tear down in ownership order");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_v2_multi_block_graph_is_durable_and_recoverable_for_mmio_and_pci() {
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootSessionConfig, HvfArm64BootStorageCaptureErrorKind,
        HvfArm64BootStorageCaptureStage, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{
        BlockMmioLayout, DriveCacheType, DriveConfigInput, DriveIoEngine, PreparedBlockDevice,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::snapshot_device_v2::{
        SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    };
    use bangbang_runtime::snapshot_device_v2_5::{
        NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2MultiBlockDeviceGraph,
    };
    use bangbang_runtime::storage_capture::{CaptureReadyStorageConfigs, StorageDeviceOrigin};
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");

    for (case, pci_enabled, expected_transport) in [
        (
            "native-v2-multi-block-mmio",
            false,
            SnapshotV2DeviceTransportKind::Mmio,
        ),
        (
            "native-v2-multi-block-pci",
            true,
            SnapshotV2DeviceTransportKind::Pci,
        ),
    ] {
        let kernel = TempFile::new(&format!("{case}-kernel"), &image)
            .unwrap_or_else(|error| panic!("{case} kernel should create: {error}"));
        let root = TempFile::new_len(&format!("{case}-root"), 4096)
            .unwrap_or_else(|error| panic!("{case} root should create: {error}"));
        let sync = TempFile::new_len(&format!("{case}-sync"), 8193)
            .unwrap_or_else(|error| panic!("{case} Sync backing should create: {error}"));
        let asynchronous = TempFile::new_len(&format!("{case}-async"), 12_288)
            .unwrap_or_else(|error| panic!("{case} Async backing should create: {error}"));
        let mut controller = bangbang_runtime::VmmController::new(case, "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                kernel.path(),
            )))
            .unwrap_or_else(|error| panic!("{case} boot source should configure: {error}"));
        controller
            .handle_action(VmmAction::PutDrive(
                DriveConfigInput::new("rootfs", "rootfs", root.path(), true)
                    .with_is_read_only(true)
                    .with_cache_type(DriveCacheType::Writeback)
                    .with_io_engine(DriveIoEngine::Sync),
            ))
            .unwrap_or_else(|error| panic!("{case} root should configure: {error}"));
        controller
            .handle_action(VmmAction::PutDrive(
                DriveConfigInput::new("data_sync", "data_sync", sync.path(), false)
                    .with_is_read_only(false)
                    .with_cache_type(DriveCacheType::Unsafe)
                    .with_io_engine(DriveIoEngine::Sync),
            ))
            .unwrap_or_else(|error| panic!("{case} Sync data should configure: {error}"));
        if !pci_enabled {
            controller
                .handle_action(VmmAction::PutDrive(
                    DriveConfigInput::new("data_async", "data_async", asynchronous.path(), false)
                        .with_is_read_only(false)
                        .with_cache_type(DriveCacheType::Writeback)
                        .with_io_engine(DriveIoEngine::Async),
                ))
                .unwrap_or_else(|error| panic!("{case} Async data should configure: {error}"));
        }

        let mut session_config = HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            test_rtc_mmio_layout(),
        );
        if pci_enabled {
            session_config = session_config.with_pci_enabled();
        }
        let mut session = OwnedHvfArm64BootSession::new(&controller, session_config)
            .unwrap_or_else(|error| panic!("{case} signed session should prepare: {error}"));

        if pci_enabled {
            let input =
                DriveConfigInput::new("data_async", "data_async", asynchronous.path(), false)
                    .with_is_read_only(false)
                    .with_cache_type(DriveCacheType::Writeback)
                    .with_io_engine(DriveIoEngine::Async);
            let config = input.clone().validate().unwrap_or_else(|error| {
                panic!("{case} runtime Async config should validate: {error}")
            });
            controller
                .handle_action(VmmAction::PutDrive(input))
                .unwrap_or_else(|error| {
                    panic!("{case} runtime Async config should publish: {error}")
                });
            session
                .insert_runtime_block_device(
                    PreparedBlockDevice::from_config_with_backing(&config, None).unwrap_or_else(
                        |error| panic!("{case} runtime Async device should prepare: {error}"),
                    ),
                )
                .unwrap_or_else(|error| {
                    panic!("{case} runtime Async device should insert: {error}")
                });
        }

        let configs =
            CaptureReadyStorageConfigs::new(controller.drive_configs().to_vec(), Vec::new());
        let guard = session
            .quiesce_limiter_retry_wakeups()
            .unwrap_or_else(|error| panic!("{case} retry publishers should quiesce: {error}"));

        for cancelled_stage in [
            HvfArm64BootStorageCaptureStage::Persist,
            HvfArm64BootStorageCaptureStage::ComposeGraph,
        ] {
            let result = session.capture_snapshot_v2_multi_block_device_graph_at_with_cancel(
                &configs,
                &guard,
                Instant::now(),
                |stage| stage == cancelled_stage,
            );
            let error = match result {
                Ok(_) => panic!("{case} cancellation at {cancelled_stage:?} returned a graph"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), HvfArm64BootStorageCaptureErrorKind::Cancelled);
            assert!(!error.cleanup_failed());
            assert!(!error.terminal());
        }

        let mut observed_stages = Vec::new();
        let first = session
            .capture_snapshot_v2_multi_block_device_graph_at_with_cancel(
                &configs,
                &guard,
                Instant::now(),
                |stage| {
                    observed_stages.push(stage);
                    false
                },
            )
            .unwrap_or_else(|error| panic!("{case} first graph should capture: {error}"));
        let last_drain = observed_stages
            .iter()
            .rposition(|stage| *stage == HvfArm64BootStorageCaptureStage::DrainAsync)
            .expect("profile capture should observe an Async drain boundary");
        let first_persist = observed_stages
            .iter()
            .position(|stage| *stage == HvfArm64BootStorageCaptureStage::Persist)
            .expect("profile capture should observe a persistence boundary");
        let first_publication = observed_stages
            .iter()
            .position(|stage| *stage == HvfArm64BootStorageCaptureStage::PublishAsync)
            .expect("profile capture should observe a publication boundary");
        let first_capture = observed_stages
            .iter()
            .position(|stage| *stage == HvfArm64BootStorageCaptureStage::Capture)
            .expect("profile capture should observe a live-state capture boundary");
        let first_composition = observed_stages
            .iter()
            .position(|stage| *stage == HvfArm64BootStorageCaptureStage::ComposeGraph)
            .expect("profile capture should observe a graph-composition boundary");
        assert!(last_drain < first_persist);
        assert!(first_persist < first_publication);
        assert!(first_publication < first_capture);
        assert!(first_capture < first_composition);
        let second = session
            .capture_snapshot_v2_multi_block_device_graph_at(&configs, &guard, Instant::now())
            .unwrap_or_else(|error| panic!("{case} second graph should capture: {error}"));
        assert_eq!(first, second);
        assert_eq!(first.transport_kind(), expected_transport);
        assert_eq!(first.records().len(), 3);
        assert_eq!(first.root_key(), Some(first.records()[0].key()));
        assert_eq!(first.records()[0].config().drive_id(), "rootfs");
        assert!(first.records()[0].config().is_read_only());
        assert_eq!(first.records()[0].config().io_engine(), DriveIoEngine::Sync);
        assert_eq!(first.records()[1].config().drive_id(), "data_sync");
        assert!(!first.records()[1].config().is_read_only());
        assert_eq!(first.records()[1].config().io_engine(), DriveIoEngine::Sync);
        assert_eq!(
            first.records()[1].config().cache_type(),
            DriveCacheType::Unsafe
        );
        assert_eq!(first.records()[1].block().backing_bytes(), 8193);
        assert_eq!(first.records()[2].config().drive_id(), "data_async");
        assert_eq!(
            first.records()[2].config().io_engine(),
            DriveIoEngine::Async
        );
        assert_eq!(
            first.records()[2].config().cache_type(),
            DriveCacheType::Writeback
        );
        match first.records()[2].transport() {
            SnapshotV2DeviceTransport::Mmio(_) => assert!(!pci_enabled),
            SnapshotV2DeviceTransport::Pci(state) => {
                assert!(pci_enabled);
                assert_eq!(state.origin(), StorageDeviceOrigin::Runtime);
            }
        }

        let bytes = first
            .encode(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION)
            .unwrap_or_else(|error| panic!("{case} graph should encode: {error}"));
        let decoded = SnapshotV2MultiBlockDeviceGraph::decode(
            NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            &bytes,
        )
        .unwrap_or_else(|error| panic!("{case} graph should decode: {error}"));
        assert_eq!(decoded, first);
        assert_eq!(
            decoded
                .encode(NATIVE_V2_MULTI_BLOCK_DEVICE_GRAPH_COMPATIBILITY_VERSION)
                .unwrap_or_else(|error| panic!("{case} decoded graph should re-encode: {error}")),
            bytes
        );
        let diagnostics = format!("{first:?} {:?}", first.records());
        for private in [
            path_text(root.path()),
            path_text(sync.path()),
            path_text(asynchronous.path()),
        ] {
            assert!(!diagnostics.contains(&private));
        }

        drop(guard);
        session
            .shutdown()
            .unwrap_or_else(|error| panic!("{case} session should shut down: {error}"));
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const NATIVE_V2_MULTI_BLOCK_QUEUE_SIZE: u16 = 8;

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone, Copy)]
struct NativeV2MultiBlockQueueFixture {
    base: bangbang_runtime::memory::GuestAddress,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl NativeV2MultiBlockQueueFixture {
    const fn new(base: u64) -> Self {
        Self {
            base: bangbang_runtime::memory::GuestAddress::new(base),
        }
    }

    fn address(self, offset: u64) -> bangbang_runtime::memory::GuestAddress {
        self.base
            .checked_add(offset)
            .expect("native-v2 multi-block fixture address should fit")
    }

    fn descriptor_table(self) -> bangbang_runtime::memory::GuestAddress {
        self.address(0)
    }

    fn available_ring(self) -> bangbang_runtime::memory::GuestAddress {
        self.address(0x1000)
    }

    fn used_ring(self) -> bangbang_runtime::memory::GuestAddress {
        self.address(0x2000)
    }

    fn write_header(self) -> bangbang_runtime::memory::GuestAddress {
        self.address(0x3000)
    }

    fn write_data(self) -> bangbang_runtime::memory::GuestAddress {
        self.address(0x4000)
    }

    fn write_status(self) -> bangbang_runtime::memory::GuestAddress {
        self.address(0x5000)
    }

    fn flush_header(self) -> bangbang_runtime::memory::GuestAddress {
        self.address(0x6000)
    }

    fn flush_status(self) -> bangbang_runtime::memory::GuestAddress {
        self.address(0x7000)
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const NATIVE_V2_MULTI_BLOCK_SYNC_QUEUE: NativeV2MultiBlockQueueFixture =
    NativeV2MultiBlockQueueFixture::new(0x8060_0000);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const NATIVE_V2_MULTI_BLOCK_ASYNC_QUEUE: NativeV2MultiBlockQueueFixture =
    NativeV2MultiBlockQueueFixture::new(0x8070_0000);

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_native_v2_multi_block_mmio(
    dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher,
    address: bangbang_runtime::memory::GuestAddress,
    data: &[u8],
) {
    use bangbang_runtime::mmio::{MmioAccessBytes, MmioDispatchOutcome, MmioOperation};

    let access = dispatcher
        .lookup(
            address,
            u64::try_from(data.len()).expect("multi-block MMIO write length should fit u64"),
        )
        .expect("multi-block MMIO write should resolve");
    let outcome = dispatcher
        .dispatch(
            MmioOperation::write(
                access,
                MmioAccessBytes::new(data).expect("multi-block MMIO bytes should validate"),
            )
            .expect("multi-block MMIO write should validate"),
        )
        .expect("multi-block MMIO write should dispatch");
    assert!(matches!(outcome, MmioDispatchOutcome::Write));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn read_native_v2_multi_block_mmio_u32(
    dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher,
    address: bangbang_runtime::memory::GuestAddress,
) -> u32 {
    use bangbang_runtime::mmio::{MmioDispatchOutcome, MmioOperation};

    let access = dispatcher
        .lookup(address, 4)
        .expect("multi-block MMIO read should resolve");
    let outcome = dispatcher
        .dispatch(MmioOperation::read(access).expect("multi-block MMIO read should validate"))
        .expect("multi-block MMIO read should dispatch");
    let data = match outcome {
        MmioDispatchOutcome::Read { data } => Some(data),
        MmioDispatchOutcome::Write => None,
    };
    let data = data.expect("multi-block MMIO read should return data");
    u32::from_le_bytes(
        data.as_slice()
            .try_into()
            .expect("multi-block MMIO u32 should contain four bytes"),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_native_v2_multi_block_descriptor(
    memory: &mut bangbang_runtime::memory::GuestMemory,
    fixture: NativeV2MultiBlockQueueFixture,
    index: u16,
    address: bangbang_runtime::memory::GuestAddress,
    len: u32,
    flags: u16,
    next: u16,
) {
    use bangbang_runtime::virtio_queue::VIRTQUEUE_DESCRIPTOR_SIZE;

    let descriptor = fixture
        .descriptor_table()
        .checked_add(
            u64::from(index)
                * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE)
                    .expect("virtqueue descriptor size should fit u64"),
        )
        .expect("multi-block descriptor address should fit");
    memory
        .write_slice(&address.raw_value().to_le_bytes(), descriptor)
        .expect("multi-block descriptor address should write");
    memory
        .write_slice(
            &len.to_le_bytes(),
            descriptor
                .checked_add(8)
                .expect("multi-block descriptor length address should fit"),
        )
        .expect("multi-block descriptor length should write");
    memory
        .write_slice(
            &flags.to_le_bytes(),
            descriptor
                .checked_add(12)
                .expect("multi-block descriptor flags address should fit"),
        )
        .expect("multi-block descriptor flags should write");
    memory
        .write_slice(
            &next.to_le_bytes(),
            descriptor
                .checked_add(14)
                .expect("multi-block descriptor next address should fit"),
        )
        .expect("multi-block descriptor next index should write");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_native_v2_multi_block_request_header(
    memory: &mut bangbang_runtime::memory::GuestMemory,
    address: bangbang_runtime::memory::GuestAddress,
    request_type: u32,
) {
    memory
        .write_slice(&request_type.to_le_bytes(), address)
        .expect("multi-block request type should write");
    memory
        .write_slice(
            &0_u32.to_le_bytes(),
            address
                .checked_add(4)
                .expect("multi-block reserved header address should fit"),
        )
        .expect("multi-block reserved header should write");
    memory
        .write_slice(
            &0_u64.to_le_bytes(),
            address
                .checked_add(8)
                .expect("multi-block sector header address should fit"),
        )
        .expect("multi-block request sector should write");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn initialize_native_v2_multi_block_queue(
    memory: &mut bangbang_runtime::memory::GuestMemory,
    fixture: NativeV2MultiBlockQueueFixture,
) {
    memory
        .write_slice(&[0; 4], fixture.available_ring())
        .expect("multi-block available ring should initialize");
    memory
        .write_slice(&[0; 4], fixture.used_ring())
        .expect("multi-block used ring should initialize");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn submit_native_v2_multi_block_write_and_flush(
    memory: &mut bangbang_runtime::memory::GuestMemory,
    fixture: NativeV2MultiBlockQueueFixture,
    payload_byte: u8,
) {
    use bangbang_runtime::block::{
        VIRTIO_BLOCK_REQUEST_HEADER_SIZE, VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
        VIRTIO_BLOCK_REQUEST_TYPE_OUT, VIRTIO_BLOCK_SECTOR_SIZE, VIRTIO_BLOCK_STATUS_SIZE,
    };
    use bangbang_runtime::virtio_queue::{VIRTQUEUE_DESC_F_NEXT, VIRTQUEUE_DESC_F_WRITE};

    write_native_v2_multi_block_request_header(
        memory,
        fixture.write_header(),
        VIRTIO_BLOCK_REQUEST_TYPE_OUT,
    );
    write_native_v2_multi_block_request_header(
        memory,
        fixture.flush_header(),
        VIRTIO_BLOCK_REQUEST_TYPE_FLUSH,
    );
    memory
        .write_slice(
            &[payload_byte; VIRTIO_BLOCK_SECTOR_SIZE as usize],
            fixture.write_data(),
        )
        .expect("multi-block write payload should write");
    memory
        .write_slice(&[u8::MAX], fixture.write_status())
        .expect("multi-block write status should initialize");
    memory
        .write_slice(&[u8::MAX], fixture.flush_status())
        .expect("multi-block flush status should initialize");

    write_native_v2_multi_block_descriptor(
        memory,
        fixture,
        0,
        fixture.write_header(),
        VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
        VIRTQUEUE_DESC_F_NEXT,
        1,
    );
    write_native_v2_multi_block_descriptor(
        memory,
        fixture,
        1,
        fixture.write_data(),
        u32::try_from(VIRTIO_BLOCK_SECTOR_SIZE).expect("block sector size should fit u32"),
        VIRTQUEUE_DESC_F_NEXT,
        2,
    );
    write_native_v2_multi_block_descriptor(
        memory,
        fixture,
        2,
        fixture.write_status(),
        VIRTIO_BLOCK_STATUS_SIZE,
        VIRTQUEUE_DESC_F_WRITE,
        0,
    );
    write_native_v2_multi_block_descriptor(
        memory,
        fixture,
        3,
        fixture.flush_header(),
        VIRTIO_BLOCK_REQUEST_HEADER_SIZE,
        VIRTQUEUE_DESC_F_NEXT,
        4,
    );
    write_native_v2_multi_block_descriptor(
        memory,
        fixture,
        4,
        fixture.flush_status(),
        VIRTIO_BLOCK_STATUS_SIZE,
        VIRTQUEUE_DESC_F_WRITE,
        0,
    );

    memory
        .write_slice(
            &0_u16.to_le_bytes(),
            fixture
                .available_ring()
                .checked_add(4)
                .expect("first multi-block available head address should fit"),
        )
        .expect("first multi-block available head should write");
    memory
        .write_slice(
            &3_u16.to_le_bytes(),
            fixture
                .available_ring()
                .checked_add(6)
                .expect("second multi-block available head address should fit"),
        )
        .expect("second multi-block available head should write");
    memory
        .write_slice(
            &2_u16.to_le_bytes(),
            fixture
                .available_ring()
                .checked_add(2)
                .expect("multi-block available index address should fit"),
        )
        .expect("multi-block available index should write");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn activate_native_v2_multi_block_queue(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
    transport_base: bangbang_runtime::memory::GuestAddress,
    fixture: NativeV2MultiBlockQueueFixture,
) {
    use bangbang_runtime::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK, VirtioMmioRegister,
    };

    let dispatcher = session.mmio_dispatcher();
    let mut dispatcher = dispatcher
        .lock()
        .expect("multi-block MMIO dispatcher should not be poisoned");
    let write = |dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher,
                 register: VirtioMmioRegister,
                 value: u32| {
        write_native_v2_multi_block_mmio(
            dispatcher,
            transport_base
                .checked_add(register.offset())
                .expect("multi-block register address should fit"),
            &value.to_le_bytes(),
        );
    };
    let features_ok = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    for status in [
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
    ] {
        write(&mut dispatcher, VirtioMmioRegister::Status, status);
    }
    write(&mut dispatcher, VirtioMmioRegister::DriverFeaturesSel, 1);
    write(&mut dispatcher, VirtioMmioRegister::DriverFeatures, 1);
    write(&mut dispatcher, VirtioMmioRegister::Status, features_ok);
    write(&mut dispatcher, VirtioMmioRegister::QueueSel, 0);
    write(
        &mut dispatcher,
        VirtioMmioRegister::QueueNum,
        u32::from(NATIVE_V2_MULTI_BLOCK_QUEUE_SIZE),
    );
    write(
        &mut dispatcher,
        VirtioMmioRegister::QueueDescLow,
        u32::try_from(fixture.descriptor_table().raw_value())
            .expect("multi-block descriptor address should fit u32"),
    );
    write(
        &mut dispatcher,
        VirtioMmioRegister::QueueDriverLow,
        u32::try_from(fixture.available_ring().raw_value())
            .expect("multi-block available ring should fit u32"),
    );
    write(
        &mut dispatcher,
        VirtioMmioRegister::QueueDeviceLow,
        u32::try_from(fixture.used_ring().raw_value())
            .expect("multi-block used ring should fit u32"),
    );
    write(&mut dispatcher, VirtioMmioRegister::QueueReady, 1);
    write(
        &mut dispatcher,
        VirtioMmioRegister::Status,
        features_ok | VIRTIO_DEVICE_STATUS_DRIVER_OK,
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn notify_native_v2_multi_block_queue(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
    transport_base: bangbang_runtime::memory::GuestAddress,
) {
    use bangbang_runtime::virtio_mmio::VirtioMmioRegister;

    let dispatcher = session.mmio_dispatcher();
    let mut dispatcher = dispatcher
        .lock()
        .expect("multi-block MMIO dispatcher should not be poisoned");
    write_native_v2_multi_block_mmio(
        &mut dispatcher,
        transport_base
            .checked_add(VirtioMmioRegister::QueueNotify.offset())
            .expect("multi-block queue-notify address should fit"),
        &0_u32.to_le_bytes(),
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn activate_native_v2_multi_block_pci_queue(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
    bar_base: bangbang_runtime::memory::GuestAddress,
    fixture: NativeV2MultiBlockQueueFixture,
    message_address: u64,
    message_data: u32,
) {
    use bangbang_runtime::virtio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK,
    };
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_MSIX_TABLE_OFFSET;

    const DRIVER_FEATURE_SELECT: u64 = 0x08;
    const DRIVER_FEATURE: u64 = 0x0c;
    const DEVICE_STATUS: u64 = 0x14;
    const QUEUE_SELECT: u64 = 0x16;
    const QUEUE_SIZE: u64 = 0x18;
    const QUEUE_MSIX_VECTOR: u64 = 0x1a;
    const QUEUE_ENABLE: u64 = 0x1c;
    const QUEUE_DESC_LOW: u64 = 0x20;
    const QUEUE_AVAIL_LOW: u64 = 0x28;
    const QUEUE_USED_LOW: u64 = 0x30;

    let dispatcher = session.mmio_dispatcher();
    let mut dispatcher = dispatcher
        .lock()
        .expect("multi-block PCI dispatcher should not be poisoned");
    let write =
        |dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher, offset: u64, data: &[u8]| {
            write_native_v2_multi_block_mmio(
                dispatcher,
                bar_base
                    .checked_add(offset)
                    .expect("multi-block PCI BAR address should fit"),
                data,
            );
        };

    write(
        &mut dispatcher,
        VIRTIO_PCI_MSIX_TABLE_OFFSET,
        &message_address.to_le_bytes(),
    );
    write(
        &mut dispatcher,
        VIRTIO_PCI_MSIX_TABLE_OFFSET + 8,
        &u64::from(message_data).to_le_bytes(),
    );
    for status in [
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
    ] {
        write(
            &mut dispatcher,
            DEVICE_STATUS,
            &[u8::try_from(status).expect("virtio status should fit u8")],
        );
    }
    write(&mut dispatcher, DRIVER_FEATURE_SELECT, &1_u32.to_le_bytes());
    write(&mut dispatcher, DRIVER_FEATURE, &1_u32.to_le_bytes());
    write(
        &mut dispatcher,
        DEVICE_STATUS,
        &[u8::try_from(
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK,
        )
        .expect("virtio status should fit u8")],
    );
    write(&mut dispatcher, QUEUE_SELECT, &0_u16.to_le_bytes());
    write(
        &mut dispatcher,
        QUEUE_SIZE,
        &NATIVE_V2_MULTI_BLOCK_QUEUE_SIZE.to_le_bytes(),
    );
    write(&mut dispatcher, QUEUE_MSIX_VECTOR, &0_u16.to_le_bytes());
    write(
        &mut dispatcher,
        QUEUE_DESC_LOW,
        &u32::try_from(fixture.descriptor_table().raw_value())
            .expect("multi-block descriptor address should fit u32")
            .to_le_bytes(),
    );
    write(
        &mut dispatcher,
        QUEUE_AVAIL_LOW,
        &u32::try_from(fixture.available_ring().raw_value())
            .expect("multi-block available ring should fit u32")
            .to_le_bytes(),
    );
    write(
        &mut dispatcher,
        QUEUE_USED_LOW,
        &u32::try_from(fixture.used_ring().raw_value())
            .expect("multi-block used ring should fit u32")
            .to_le_bytes(),
    );
    write(&mut dispatcher, QUEUE_ENABLE, &1_u16.to_le_bytes());
    write(
        &mut dispatcher,
        DEVICE_STATUS,
        &[u8::try_from(
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
                | VIRTIO_DEVICE_STATUS_DRIVER
                | VIRTIO_DEVICE_STATUS_FEATURES_OK
                | VIRTIO_DEVICE_STATUS_DRIVER_OK,
        )
        .expect("virtio status should fit u8")],
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn notify_native_v2_multi_block_pci_queue(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
    bar_base: bangbang_runtime::memory::GuestAddress,
) {
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_NOTIFICATION_OFFSET;

    let dispatcher = session.mmio_dispatcher();
    let mut dispatcher = dispatcher
        .lock()
        .expect("multi-block PCI dispatcher should not be poisoned");
    write_native_v2_multi_block_mmio(
        &mut dispatcher,
        bar_base
            .checked_add(VIRTIO_PCI_NOTIFICATION_OFFSET)
            .expect("multi-block PCI notification address should fit"),
        &0_u16.to_le_bytes(),
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn native_v2_multi_block_interrupt_status(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
    transport_base: bangbang_runtime::memory::GuestAddress,
) -> u32 {
    use bangbang_runtime::virtio_mmio::VirtioMmioRegister;

    let dispatcher = session.mmio_dispatcher();
    let mut dispatcher = dispatcher
        .lock()
        .expect("multi-block MMIO dispatcher should not be poisoned");
    read_native_v2_multi_block_mmio_u32(
        &mut dispatcher,
        transport_base
            .checked_add(VirtioMmioRegister::InterruptStatus.offset())
            .expect("multi-block interrupt-status address should fit"),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn read_native_v2_multi_block_guest_u16(
    memory: &bangbang_runtime::memory::GuestMemory,
    address: bangbang_runtime::memory::GuestAddress,
) -> u16 {
    let mut bytes = [0_u8; 2];
    memory
        .read_slice(&mut bytes, address)
        .expect("multi-block guest u16 should read");
    u16::from_le_bytes(bytes)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn read_native_v2_multi_block_guest_u8(
    memory: &bangbang_runtime::memory::GuestMemory,
    address: bangbang_runtime::memory::GuestAddress,
) -> u8 {
    let mut byte = [0_u8; 1];
    memory
        .read_slice(&mut byte, address)
        .expect("multi-block guest u8 should read");
    byte.first()
        .copied()
        .expect("multi-block guest u8 should contain one byte")
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn native_v2_multi_block_used_index(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
    fixture: NativeV2MultiBlockQueueFixture,
) -> u16 {
    read_native_v2_multi_block_guest_u16(
        session
            .guest_memory()
            .expect("multi-block guest memory should remain mapped"),
        fixture
            .used_ring()
            .checked_add(2)
            .expect("multi-block used index address should fit"),
    )
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn wait_for_native_v2_multi_block_async_completion(
    session: &bangbang_hvf::OwnedHvfArm64BootSession,
) {
    let completion_fd = session
        .runtime_resources()
        .block_async_completion_fd()
        .expect("multi-block Async completion descriptor should inspect")
        .expect("multi-block Async runtime should expose a completion descriptor");
    let mut readiness = libc::pollfd {
        fd: completion_fd,
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: One initialized pollfd is writable for the bounded wait.
    let ready = unsafe { libc::poll(&raw mut readiness, 1, 5_000) };
    assert_eq!(
        ready, 1,
        "multi-block Async request should complete before the deadline"
    );
    assert_ne!(
        readiness.revents & libc::POLLIN,
        0,
        "multi-block Async completion descriptor should become readable"
    );
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn native_v2_multi_block_dispatch_signaled(
    dispatches: &bangbang_hvf::HvfArm64BootBlockNotificationDispatches,
    drive_id: &str,
) -> bool {
    dispatches.as_slice().iter().any(|dispatch| {
        dispatch.dispatch().device().registration.drive_id() == drive_id
            && dispatch.queue_interrupt_signaled()
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_v2_multi_block_reconstructs_rooted_and_rootless_mmio_and_pci_owners() {
    use std::fs::OpenOptions;
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSnapshotV2CaptureInput, HvfSnapshotV2BootState,
        HvfSnapshotV2DefaultProcessShell, HvfSnapshotV2MultiBlockProcessConfig,
        HvfSnapshotV2NativePath, HvfVcpuRunStepOutcome, OwnedHvfArm64BootSession,
        prepare_hvf_snapshot_v2_multi_block_platform_plan,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{
        BlockFileBacking, BlockMmioLayout, DriveCacheType, DriveConfigInput, DriveIoEngine,
        PreparedBlockDevice,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::serial::{SharedSerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceTransport;
    use bangbang_runtime::snapshot_device_v2_5::SnapshotV2MultiBlockRestorePlan;
    use bangbang_runtime::storage_capture::{CaptureReadyStorageConfigs, StorageDeviceOrigin};
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");

    for (case, rooted, pci_enabled) in [
        ("native-v2-multi-block-mmio-rooted-owner", true, false),
        ("native-v2-multi-block-mmio-rootless-owner", false, false),
        ("native-v2-multi-block-pci-rooted-owner", true, true),
        ("native-v2-multi-block-pci-rootless-owner", false, true),
    ] {
        let kernel = TempFile::new(&format!("{case}-kernel"), &image)
            .unwrap_or_else(|error| panic!("{case} kernel should create: {error}"));
        let first = TempFile::new_len(&format!("{case}-sync"), 4096)
            .unwrap_or_else(|error| panic!("{case} Sync backing should create: {error}"));
        let second = TempFile::new_len(&format!("{case}-async"), 4096)
            .unwrap_or_else(|error| panic!("{case} Async backing should create: {error}"));
        let mut controller = bangbang_runtime::VmmController::new(case, "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                kernel.path(),
            )))
            .unwrap_or_else(|error| panic!("{case} boot source should configure: {error}"));
        controller
            .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(1, 16)))
            .unwrap_or_else(|error| panic!("{case} machine should configure: {error}"));
        controller
            .handle_action(VmmAction::PutDrive(
                DriveConfigInput::new("sync", "sync", first.path(), rooted)
                    .with_is_read_only(false)
                    .with_cache_type(DriveCacheType::Unsafe)
                    .with_io_engine(DriveIoEngine::Sync),
            ))
            .unwrap_or_else(|error| panic!("{case} Sync drive should configure: {error}"));
        let async_input = DriveConfigInput::new("async", "async", second.path(), false)
            .with_is_read_only(false)
            .with_cache_type(DriveCacheType::Writeback)
            .with_io_engine(DriveIoEngine::Async);
        if !pci_enabled {
            controller
                .handle_action(VmmAction::PutDrive(async_input.clone()))
                .unwrap_or_else(|error| panic!("{case} Async drive should configure: {error}"));
        }

        let block_layout =
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1));
        let source_serial = SharedSerialOutputBuffer::default();
        let mut session_config = HvfArm64BootSessionConfig::new(
            block_layout,
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            bangbang_runtime::rtc::RtcMmioLayout::new(
                GuestAddress::new(0x4000_1000),
                MmioRegionId::new(10),
            ),
        )
        .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
            MmioRegionId::new(20),
            GuestAddress::new(0x4000_2000),
            SharedSerialOutput::from(source_serial),
        ));
        if pci_enabled {
            session_config = session_config.with_pci_enabled();
        }
        let mut source = OwnedHvfArm64BootSession::new(&controller, session_config)
            .unwrap_or_else(|error| panic!("{case} source should prepare: {error}"));
        if pci_enabled {
            let async_config = async_input
                .clone()
                .validate()
                .unwrap_or_else(|error| panic!("{case} Async config should validate: {error}"));
            controller
                .handle_action(VmmAction::PutDrive(async_input))
                .unwrap_or_else(|error| panic!("{case} Async drive should configure: {error}"));
            source
                .insert_runtime_block_device(
                    PreparedBlockDevice::from_config_with_backing(&async_config, None)
                        .unwrap_or_else(|error| {
                            panic!("{case} runtime Async drive should prepare: {error}")
                        }),
                )
                .unwrap_or_else(|error| {
                    panic!("{case} runtime Async drive should publish: {error}")
                });
        }

        let source_entry = GuestAddress::new(
            source
                .capture_arm64_general_register_state()
                .unwrap_or_else(|error| panic!("{case} source registers should capture: {error}"))
                .pc(),
        );
        let guest_code = [
            0xd503_4fdf, // msr daifset, #0xf (synthetic guest has no exception vectors)
            0xd282_4685, // mov x5, #0x1234
            0xd2a8_0001, // mov x1, #0x40000000
            0xf284_00e1, // movk x1, #0x2007
            0xd280_0b42, // mov x2, #0x5a
            0x3900_0022, // strb w2, [x1]
            0xd2b0_8000, // mov x0, #0x84000000 (PSCI_VERSION)
            0xd400_0002, // hvc #0
            0xd28a_cf06, // mov x6, #0x5678
            0xd2b0_8000, // mov x0, #0x84000000 (PSCI_VERSION)
            0xd400_0002, // hvc #0
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect::<Vec<_>>();
        source
            .guest_memory_mut()
            .unwrap_or_else(|error| panic!("{case} source memory should map: {error}"))
            .write_slice(&guest_code, source_entry)
            .unwrap_or_else(|error| panic!("{case} guest continuation should write: {error}"));
        assert!(matches!(
            source.run_once_and_handle_mmio(),
            Ok(HvfVcpuRunStepOutcome::Mmio { .. })
        ));
        assert!(matches!(
            source.run_once_and_handle_mmio(),
            Ok(HvfVcpuRunStepOutcome::Hvc {
                function_id: 0x8400_0000,
                return_value: 0x0001_0000,
                ..
            })
        ));
        assert_eq!(
            source
                .capture_arm64_general_register_state()
                .unwrap_or_else(|error| {
                    panic!("{case} masked source registers should capture: {error}")
                })
                .cpsr()
                & 0xc0,
            0xc0,
            "{case} synthetic source guest should mask IRQ and FIQ"
        );

        let source_configs =
            CaptureReadyStorageConfigs::new(controller.drive_configs().to_vec(), Vec::new());
        let source_guard = source
            .quiesce_limiter_retry_wakeups()
            .unwrap_or_else(|error| panic!("{case} retry publishers should quiesce: {error}"));
        let graph = source
            .capture_snapshot_v2_multi_block_device_graph_at(
                &source_configs,
                &source_guard,
                Instant::now(),
            )
            .unwrap_or_else(|error| panic!("{case} graph should capture: {error}"));
        assert_eq!(graph.records().len(), 2);
        assert_eq!(graph.root_key().is_some(), rooted);
        if pci_enabled {
            let SnapshotV2DeviceTransport::Pci(second_transport) = graph.records()[1].transport()
            else {
                panic!("{case} runtime record should use PCI");
            };
            assert_eq!(second_transport.origin(), StorageDeviceOrigin::Runtime);
        }
        source
            .pause_for_snapshot_v2_capture()
            .unwrap_or_else(|error| panic!("{case} source should pause: {error}"));
        let boot = HvfSnapshotV2BootState::try_new(
            HvfSnapshotV2NativePath::try_new(kernel.path().as_os_str())
                .unwrap_or_else(|error| panic!("{case} kernel path should validate: {error}")),
            None,
            None,
        )
        .unwrap_or_else(|error| panic!("{case} boot metadata should validate: {error}"));
        let capture_input = HvfArm64BootSnapshotV2CaptureInput::new(boot);
        let memory_artifact = TempFile::new_len(&format!("{case}-memory"), 0)
            .unwrap_or_else(|error| panic!("{case} memory artifact should create: {error}"));
        let mut memory_writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(memory_artifact.path())
            .unwrap_or_else(|error| panic!("{case} memory artifact should open: {error}"));
        let source_platform = source
            .capture_snapshot_v2_device_graph_platform_with_cancel(
                capture_input.clone(),
                &mut memory_writer,
                |_| false,
            )
            .unwrap_or_else(|error| panic!("{case} platform should capture: {error}"));

        let layout = GuestMemoryLayout::new(source.runtime_resources().layout.ranges().to_vec())
            .unwrap_or_else(|error| panic!("{case} destination layout should validate: {error}"));
        let mut restored_memory = GuestMemory::allocate(&layout)
            .unwrap_or_else(|error| panic!("{case} destination memory should allocate: {error}"));
        let source_memory = source
            .guest_memory()
            .unwrap_or_else(|error| panic!("{case} source memory should remain mapped: {error}"));
        let mut page = vec![0_u8; 64 * 1024];
        for range in layout.ranges() {
            let mut copied = 0_u64;
            while copied < range.size() {
                let remaining = range.size() - copied;
                let count = usize::try_from(
                    remaining.min(u64::try_from(page.len()).expect("page length should fit")),
                )
                .expect("copy count should fit");
                let address = range
                    .start()
                    .checked_add(copied)
                    .expect("copy address should fit");
                source_memory
                    .read_slice(&mut page[..count], address)
                    .unwrap_or_else(|error| panic!("{case} source page should read: {error}"));
                restored_memory
                    .write_slice(&page[..count], address)
                    .unwrap_or_else(|error| {
                        panic!("{case} destination page should write: {error}")
                    });
                copied += u64::try_from(count).expect("copy count should fit u64");
            }
        }
        drop(memory_writer);
        drop(source_guard);
        source
            .shutdown()
            .unwrap_or_else(|error| panic!("{case} source should shut down: {error}"));

        let drive_configs = graph
            .project_drive_configs()
            .unwrap_or_else(|error| panic!("{case} configs should project: {error}"));
        let backings = graph
            .records()
            .iter()
            .map(|record| {
                BlockFileBacking::open_snapshot(
                    std::path::Path::new(record.config().selector()),
                    record.config().is_read_only(),
                )
                .map(|(backing, _identity)| backing)
                .unwrap_or_else(|error| panic!("{case} backing should reopen: {error}"))
            })
            .collect::<Vec<_>>();
        let now = Instant::now();
        let bundle = SnapshotV2MultiBlockRestorePlan::prepare(graph.clone(), &restored_memory, now)
            .unwrap_or_else(|error| panic!("{case} restore plan should prepare: {error}"))
            .prepare_backings(drive_configs.clone(), backings)
            .unwrap_or_else(|error| panic!("{case} backing vector should prepare: {error}"));
        let platform_plan = prepare_hvf_snapshot_v2_multi_block_platform_plan(
            &source_platform,
            &bundle,
            HvfSnapshotV2MultiBlockProcessConfig::new(block_layout, pci_enabled),
        )
        .unwrap_or_else(|error| panic!("{case} platform plan should prepare: {error}"));
        let restored_serial = SharedSerialOutputBuffer::default();
        let shell =
            HvfSnapshotV2DefaultProcessShell::new(SharedSerialOutput::from(restored_serial));
        let (mut restored, restored_drive_configs) = if pci_enabled {
            let owners = OwnedHvfArm64BootSession::restore_snapshot_v2_multi_block_pci(
                source_platform.clone(),
                restored_memory,
                shell,
                bundle,
                platform_plan,
            )
            .unwrap_or_else(|error| panic!("{case} PCI owner vector should restore: {error:?}"));
            assert_eq!(owners.drive_configs(), &drive_configs);
            owners.into_parts()
        } else {
            let owners = OwnedHvfArm64BootSession::restore_snapshot_v2_multi_block_mmio(
                source_platform.clone(),
                restored_memory,
                shell,
                bundle,
                platform_plan,
            )
            .unwrap_or_else(|error| panic!("{case} MMIO owner vector should restore: {error:?}"));
            assert_eq!(owners.drive_configs(), &drive_configs);
            owners.into_parts()
        };
        assert_eq!(
            restored
                .capture_arm64_general_register_state()
                .unwrap_or_else(|error| {
                    panic!("{case} masked restored registers should capture: {error}")
                })
                .cpsr()
                & 0xc0,
            0xc0,
            "{case} restored synthetic guest should retain IRQ and FIQ masks"
        );
        assert_eq!(restored.uses_pci_data_devices(), pci_enabled);
        if pci_enabled {
            assert!(restored.runtime_resources().block_devices.is_empty());
            assert!(restored.runtime_resources().pci_block_devices.is_empty());
            assert!(restored.block_interrupt_lines().is_empty());
            let diagnostics = restored
                .pci_data_device_diagnostics()
                .expect("restored PCI manager should exist")
                .unwrap_or_else(|error| panic!("{case} PCI diagnostics should inspect: {error}"));
            assert_eq!(diagnostics.len(), 2);
            for (diagnostics, config) in diagnostics.iter().zip(restored_drive_configs.as_slice()) {
                assert_eq!(
                    diagnostics.kind,
                    bangbang_hvf::HvfArm64BootPciDataDeviceKind::Block
                );
                assert_eq!(diagnostics.id, config.drive_id());
                assert_eq!(
                    diagnostics.transport.phase,
                    bangbang_runtime::virtio_pci::VirtioPciEndpointPhase::Active
                );
                assert!(!diagnostics.transport.device_activated);
                assert_eq!(diagnostics.queue_deliveries, 0);
            }
            assert_eq!(
                restored
                    .runtime_resources()
                    .block_async_runtime
                    .generation_count()
                    .expect("restored PCI Async runtime should inspect"),
                1
            );
        } else {
            assert_eq!(restored.runtime_resources().block_devices.len(), 2);
            for ((device, record), config) in restored
                .runtime_resources()
                .block_devices
                .iter()
                .zip(graph.records())
                .zip(restored_drive_configs.as_slice())
            {
                let SnapshotV2DeviceTransport::Mmio(mmio) = record.transport() else {
                    panic!("{case} record should retain MMIO transport");
                };
                assert_eq!(
                    device.registration.index(),
                    record.key().instance() as usize
                );
                assert_eq!(device.registration.drive_id(), config.drive_id());
                assert_eq!(device.registration.region(), mmio.region());
                assert_eq!(device.fdt_device.interrupt_line, mmio.interrupt_line());
            }
        }

        let metrics = restored.shared_block_device_metrics();
        for config in restored_drive_configs.as_slice() {
            assert!(
                metrics
                    .per_drive(config.drive_id())
                    .expect("restored per-drive metrics entry should exist")
                    .snapshot()
                    .is_empty()
            );
        }
        assert_eq!(
            metrics
                .per_drive(restored_drive_configs.as_slice()[0].drive_id())
                .expect("first metrics entry should exist")
                .snapshot()
                .update_count(),
            0
        );

        if !pci_enabled {
            let dispatcher = restored.mmio_dispatcher();
            let mut dispatcher = dispatcher
                .lock()
                .expect("restored MMIO dispatcher should lock");
            for config in restored_drive_configs.as_slice() {
                let binding = restored
                    .runtime_resources()
                    .capture_ready_mmio_block_async_binding(&mut dispatcher, config.drive_id())
                    .unwrap_or_else(|error| {
                        panic!("{case} Async identity should inspect: {error}")
                    });
                match config.io_engine() {
                    Some(DriveIoEngine::Sync) => assert!(binding.is_none()),
                    Some(DriveIoEngine::Async) => {
                        let (runtime, _generation) =
                            binding.expect("Async record should retain one generation");
                        assert!(
                            runtime.same_runtime(&restored.runtime_resources().block_async_runtime)
                        );
                    }
                    None => panic!("{case} restored local drive should have an I/O engine"),
                }
                let persistence = restored
                    .runtime_resources()
                    .capture_ready_mmio_block_snapshot_persistence_binding(&mut dispatcher, config)
                    .unwrap_or_else(|error| {
                        panic!(
                            "{case} {} persistence binding should validate: {error}",
                            config.drive_id()
                        )
                    });
                assert!(!persistence.is_read_only());
                assert_eq!(Some(persistence.io_engine()), config.io_engine());
            }
        }

        let restored_configs =
            CaptureReadyStorageConfigs::new(restored_drive_configs.as_slice().to_vec(), Vec::new());
        let restored_guard = restored
            .quiesce_limiter_retry_wakeups()
            .unwrap_or_else(|error| {
                panic!("{case} restored retry publishers should quiesce: {error}")
            });
        let recaptured_graph = restored
            .capture_snapshot_v2_multi_block_device_graph_at(
                &restored_configs,
                &restored_guard,
                now,
            )
            .unwrap_or_else(|error| panic!("{case} restored graph should recapture: {error}"));
        assert_eq!(recaptured_graph, graph);
        let recaptured_memory = TempFile::new_len(&format!("{case}-recapture"), 0)
            .unwrap_or_else(|error| panic!("{case} recapture artifact should create: {error}"));
        let mut recaptured_writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(recaptured_memory.path())
            .unwrap_or_else(|error| panic!("{case} recapture artifact should open: {error}"));
        let recaptured_platform = restored
            .capture_snapshot_v2_device_graph_platform_with_cancel(
                capture_input,
                &mut recaptured_writer,
                |_| false,
            )
            .unwrap_or_else(|error| panic!("{case} platform should recapture: {error}"));
        assert_native_v2_platform_recapture_equivalent(&source_platform, &recaptured_platform);
        drop(restored_guard);

        let mut transport_bases = graph
            .records()
            .iter()
            .map(|record| match record.transport() {
                SnapshotV2DeviceTransport::Mmio(mmio) if !pci_enabled => {
                    mmio.region().range().start()
                }
                SnapshotV2DeviceTransport::Pci(pci) if pci_enabled => pci.bar_range().start(),
                _ => panic!("{case} restored test record should use the selected transport"),
            });
        let sync_transport_base = transport_bases
            .next()
            .expect("Sync transport base should exist");
        let async_transport_base = transport_bases
            .next()
            .expect("Async transport base should exist");
        assert!(
            transport_bases.next().is_none(),
            "{case} should restore exactly two block transports"
        );
        {
            let memory = restored
                .guest_memory_mut()
                .unwrap_or_else(|error| panic!("{case} restored memory should map: {error}"));
            initialize_native_v2_multi_block_queue(memory, NATIVE_V2_MULTI_BLOCK_SYNC_QUEUE);
            initialize_native_v2_multi_block_queue(memory, NATIVE_V2_MULTI_BLOCK_ASYNC_QUEUE);
        }
        if pci_enabled {
            let msi = restored
                .gic_metadata()
                .msi
                .expect("restored PCI GIC should retain MSI metadata");
            let message_address = msi
                .region
                .base
                .checked_add(bangbang_runtime::fdt::ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
                .expect("GICv2m message address should fit");
            activate_native_v2_multi_block_pci_queue(
                &restored,
                sync_transport_base,
                NATIVE_V2_MULTI_BLOCK_SYNC_QUEUE,
                message_address,
                msi.interrupt_range.base,
            );
            activate_native_v2_multi_block_pci_queue(
                &restored,
                async_transport_base,
                NATIVE_V2_MULTI_BLOCK_ASYNC_QUEUE,
                message_address,
                msi.interrupt_range
                    .base
                    .checked_add(1)
                    .expect("second GICv2m INTID should fit"),
            );
        } else {
            activate_native_v2_multi_block_queue(
                &restored,
                sync_transport_base,
                NATIVE_V2_MULTI_BLOCK_SYNC_QUEUE,
            );
            activate_native_v2_multi_block_queue(
                &restored,
                async_transport_base,
                NATIVE_V2_MULTI_BLOCK_ASYNC_QUEUE,
            );
        }

        {
            let memory = restored
                .guest_memory_mut()
                .unwrap_or_else(|error| panic!("{case} restored memory should map: {error}"));
            submit_native_v2_multi_block_write_and_flush(
                memory,
                NATIVE_V2_MULTI_BLOCK_SYNC_QUEUE,
                0x5a,
            );
        }
        if pci_enabled {
            notify_native_v2_multi_block_pci_queue(&restored, sync_transport_base);
        } else {
            notify_native_v2_multi_block_queue(&restored, sync_transport_base);
        }
        let sync_dispatches = restored
            .dispatch_block_queue_notifications_and_signal_interrupts()
            .unwrap_or_else(|error| panic!("{case} Sync requests should dispatch: {error}"));
        if pci_enabled {
            assert!(sync_dispatches.is_empty());
            let diagnostics = restored
                .pci_data_device_diagnostics()
                .expect("restored PCI manager should exist")
                .unwrap_or_else(|error| panic!("{case} PCI diagnostics should inspect: {error}"));
            assert_eq!(diagnostics[0].queue_deliveries, 1);
            assert_eq!(diagnostics[1].queue_deliveries, 0);
        } else {
            assert_eq!(sync_dispatches.len(), 2);
            assert!(native_v2_multi_block_dispatch_signaled(
                &sync_dispatches,
                restored_drive_configs.as_slice()[0].drive_id(),
            ));
            assert!(!native_v2_multi_block_dispatch_signaled(
                &sync_dispatches,
                restored_drive_configs.as_slice()[1].drive_id(),
            ));
        }
        assert_eq!(
            native_v2_multi_block_used_index(&restored, NATIVE_V2_MULTI_BLOCK_SYNC_QUEUE),
            2
        );
        {
            let memory = restored
                .guest_memory()
                .unwrap_or_else(|error| panic!("{case} restored memory should map: {error}"));
            assert_eq!(
                read_native_v2_multi_block_guest_u8(
                    memory,
                    NATIVE_V2_MULTI_BLOCK_SYNC_QUEUE.write_status(),
                ),
                bangbang_runtime::block::VIRTIO_BLOCK_STATUS_OK
            );
            assert_eq!(
                read_native_v2_multi_block_guest_u8(
                    memory,
                    NATIVE_V2_MULTI_BLOCK_SYNC_QUEUE.flush_status(),
                ),
                bangbang_runtime::block::VIRTIO_BLOCK_STATUS_OK
            );
        }

        {
            let memory = restored
                .guest_memory_mut()
                .unwrap_or_else(|error| panic!("{case} restored memory should map: {error}"));
            submit_native_v2_multi_block_write_and_flush(
                memory,
                NATIVE_V2_MULTI_BLOCK_ASYNC_QUEUE,
                0xa5,
            );
        }
        if pci_enabled {
            notify_native_v2_multi_block_pci_queue(&restored, async_transport_base);
        } else {
            notify_native_v2_multi_block_queue(&restored, async_transport_base);
        }
        let initial_async_dispatches = restored
            .dispatch_block_queue_notifications_and_signal_interrupts()
            .unwrap_or_else(|error| panic!("{case} Async requests should schedule: {error}"));
        let mut async_interrupt_signaled = if pci_enabled {
            assert!(initial_async_dispatches.is_empty());
            restored
                .pci_data_device_diagnostics()
                .expect("restored PCI manager should exist")
                .unwrap_or_else(|error| panic!("{case} PCI diagnostics should inspect: {error}"))[1]
                .queue_deliveries
                > 0
        } else {
            native_v2_multi_block_dispatch_signaled(
                &initial_async_dispatches,
                restored_drive_configs.as_slice()[1].drive_id(),
            )
        };
        for _ in 0..8 {
            if native_v2_multi_block_used_index(&restored, NATIVE_V2_MULTI_BLOCK_ASYNC_QUEUE) == 2 {
                break;
            }
            wait_for_native_v2_multi_block_async_completion(&restored);
            let dispatches = restored
                .dispatch_block_queue_notifications_and_signal_interrupts()
                .unwrap_or_else(|error| panic!("{case} Async completion should dispatch: {error}"));
            if pci_enabled {
                assert!(dispatches.is_empty());
                async_interrupt_signaled |= restored
                    .pci_data_device_diagnostics()
                    .expect("restored PCI manager should exist")
                    .unwrap_or_else(|error| {
                        panic!("{case} PCI diagnostics should inspect: {error}")
                    })[1]
                    .queue_deliveries
                    > 0;
            } else {
                async_interrupt_signaled |= native_v2_multi_block_dispatch_signaled(
                    &dispatches,
                    restored_drive_configs.as_slice()[1].drive_id(),
                );
            }
        }
        assert_eq!(
            native_v2_multi_block_used_index(&restored, NATIVE_V2_MULTI_BLOCK_ASYNC_QUEUE),
            2,
            "{case} Async write and flush should both publish"
        );
        assert!(
            async_interrupt_signaled,
            "{case} Async completion should signal its queue interrupt"
        );
        {
            let memory = restored
                .guest_memory()
                .unwrap_or_else(|error| panic!("{case} restored memory should map: {error}"));
            assert_eq!(
                read_native_v2_multi_block_guest_u8(
                    memory,
                    NATIVE_V2_MULTI_BLOCK_ASYNC_QUEUE.write_status(),
                ),
                bangbang_runtime::block::VIRTIO_BLOCK_STATUS_OK
            );
            assert_eq!(
                read_native_v2_multi_block_guest_u8(
                    memory,
                    NATIVE_V2_MULTI_BLOCK_ASYNC_QUEUE.flush_status(),
                ),
                bangbang_runtime::block::VIRTIO_BLOCK_STATUS_OK
            );
        }

        if pci_enabled {
            let diagnostics = restored
                .pci_data_device_diagnostics()
                .expect("restored PCI manager should exist")
                .unwrap_or_else(|error| panic!("{case} PCI diagnostics should inspect: {error}"));
            for endpoint in diagnostics {
                assert!(endpoint.transport.device_activated);
                assert!(endpoint.transport.driver_ready);
                assert!(endpoint.transport.msix_enabled);
                assert_eq!(endpoint.transport.programmed_msix_entries, 1);
                assert_eq!(endpoint.transport.unmasked_msix_entries, 1);
                assert_eq!(endpoint.transport.queue_vectors, [Some(0)]);
                assert!(endpoint.queue_deliveries > 0);
            }
        } else {
            let queue_interrupt = bangbang_runtime::interrupt::DeviceInterruptKind::Queue
                .status()
                .bits();
            assert_eq!(
                native_v2_multi_block_interrupt_status(&restored, sync_transport_base)
                    & queue_interrupt,
                queue_interrupt
            );
            assert_eq!(
                native_v2_multi_block_interrupt_status(&restored, async_transport_base)
                    & queue_interrupt,
                queue_interrupt
            );
        }

        for (config, expected_byte) in restored_drive_configs.as_slice().iter().zip([0x5a, 0xa5]) {
            let snapshot = metrics
                .per_drive(config.drive_id())
                .expect("restored per-drive metrics entry should exist")
                .snapshot();
            assert_eq!(snapshot.write_count(), 1);
            assert_eq!(
                snapshot.write_bytes(),
                bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE
            );
            assert_eq!(snapshot.flush_count(), 1);
            let backing = std::fs::read(
                config
                    .path_on_host()
                    .expect("restored local backing path should exist"),
            )
            .unwrap_or_else(|error| panic!("{case} backing should read: {error}"));
            assert_eq!(
                backing
                    .get(..bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE as usize)
                    .expect("restored backing should contain one sector"),
                &[expected_byte; bangbang_runtime::block::VIRTIO_BLOCK_SECTOR_SIZE as usize]
            );
        }

        let post_io_guard = restored
            .quiesce_limiter_retry_wakeups()
            .unwrap_or_else(|error| {
                panic!("{case} post-I/O retry publishers should quiesce: {error}")
            });
        let post_io_now = Instant::now();
        let post_io_graph = restored
            .capture_snapshot_v2_multi_block_device_graph_at(
                &restored_configs,
                &post_io_guard,
                post_io_now,
            )
            .unwrap_or_else(|error| panic!("{case} post-I/O graph should capture: {error}"));
        let continued_post_io_graph = restored
            .capture_snapshot_v2_multi_block_device_graph_at(
                &restored_configs,
                &post_io_guard,
                post_io_now,
            )
            .unwrap_or_else(|error| {
                panic!("{case} continued post-I/O graph should capture: {error}")
            });
        assert_ne!(post_io_graph, graph);
        assert_eq!(continued_post_io_graph, post_io_graph);
        for (index, record) in post_io_graph.records().iter().enumerate() {
            assert!(record.virtio().is_activated());
            assert!(
                record
                    .virtio()
                    .queues()
                    .first()
                    .expect("restored block queue should exist")
                    .ready()
            );
            let queue = record
                .block()
                .continuation()
                .active_queue()
                .expect("restored block queue cursor should capture");
            assert_eq!(queue.next_available(), 2);
            assert_eq!(queue.next_used(), 2);
            if pci_enabled {
                let SnapshotV2DeviceTransport::Pci(pci) = record.transport() else {
                    panic!("{case} post-I/O record should retain PCI");
                };
                let SnapshotV2DeviceTransport::Pci(original) = graph.records()[index].transport()
                else {
                    panic!("{case} original record should retain PCI");
                };
                assert_eq!(pci.origin(), original.origin());
                assert_eq!(pci.sbdf(), original.sbdf());
                assert_eq!(pci.bar_range(), original.bar_range());
                assert_eq!(pci.msix().queue_vectors(), [0]);
                let entry = pci.msix().entries()[0];
                let msi = restored
                    .gic_metadata()
                    .msi
                    .expect("restored PCI GIC should retain MSI metadata");
                let message_address = msi
                    .region
                    .base
                    .checked_add(bangbang_runtime::fdt::ARM64_GICV2M_MSI_SET_SPI_NSR_OFFSET)
                    .expect("GICv2m message address should fit");
                assert_eq!(
                    entry.message_address_low(),
                    u32::try_from(message_address & u64::from(u32::MAX))
                        .expect("GICv2m message low word should fit")
                );
                assert_eq!(
                    entry.message_address_high(),
                    u32::try_from(message_address >> 32)
                        .expect("GICv2m message high word should fit")
                );
                assert_eq!(
                    entry.message_data(),
                    msi.interrupt_range
                        .base
                        .checked_add(u32::try_from(index).expect("record index should fit"))
                        .expect("GICv2m message INTID should fit")
                );
                assert_eq!(entry.vector_control(), 0);
            }
        }
        drop(post_io_guard);

        let resumed_step = restored.run_once_and_handle_mmio();
        assert!(
            matches!(
                &resumed_step,
                Ok(HvfVcpuRunStepOutcome::Hvc {
                    function_id: 0x8400_0000,
                    return_value: 0x0001_0000,
                    ..
                })
            ),
            "{case} should resume to the second HVC after block I/O: {resumed_step:?}"
        );
        assert_eq!(
            restored
                .capture_arm64_general_register_state()
                .unwrap_or_else(|error| {
                    panic!("{case} restored registers should capture: {error}")
                })
                .general_purpose_register(6),
            Some(0x5678)
        );
        restored
            .shutdown()
            .unwrap_or_else(|error| panic!("{case} restored owner should shut down: {error}"));
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_v2_root_graph_converts_signed_mmio_and_pci_owners_canonically() {
    use std::fs::{File, OpenOptions};
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSnapshotV2CaptureInput, HvfSnapshotV2BootState,
        HvfSnapshotV2DefaultProcessShell, HvfSnapshotV2NativePath, HvfSnapshotV2RootProcessConfig,
        HvfSnapshotV2State, OwnedHvfArm64BootSession, decode_hvf_snapshot_v2_state,
        encode_hvf_snapshot_v2_state, prepare_hvf_snapshot_v2_root_plan,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{
        BlockFileBacking, BlockMmioLayout, DriveConfigInput, DriveIoEngine,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::{GuestAddress, GuestMemoryRange};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pci::{
        PCI_BAR64_START, PCI_BUS_ZERO, PCI_FIRST_ENDPOINT_DEVICE, PCI_FUNCTION_ZERO,
        PCI_SEGMENT_ZERO, PciSbdf,
    };
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::rtc::RtcMmioLayout;
    use bangbang_runtime::serial::{SharedSerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::snapshot_device_v2::{
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION, SnapshotV2DeviceGraph,
        SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    };
    use bangbang_runtime::snapshot_format_v2::decode_snapshot_v2_state_with_compatibility_version;
    use bangbang_runtime::snapshot_memory_v2::load_snapshot_v2_memory_file;
    use bangbang_runtime::storage_capture::{CaptureReadyStorageConfigs, StorageDeviceOrigin};
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");

    for (case, pci_enabled, expected_transport) in [
        (
            "native-v2-root-mmio",
            false,
            SnapshotV2DeviceTransportKind::Mmio,
        ),
        (
            "native-v2-root-pci",
            true,
            SnapshotV2DeviceTransportKind::Pci,
        ),
    ] {
        let kernel = TempFile::new(&format!("{case}-kernel"), &image)
            .unwrap_or_else(|error| panic!("{case} kernel should create: {error}"));
        let root = TempFile::new_len(&format!("{case}-backing"), 4096)
            .unwrap_or_else(|error| panic!("{case} root should create: {error}"));
        let mut controller = bangbang_runtime::VmmController::new(case, "0.1.0", "bangbang");
        controller
            .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
                kernel.path(),
            )))
            .unwrap_or_else(|error| panic!("{case} boot source should configure: {error}"));
        controller
            .handle_action(VmmAction::PutDrive(
                DriveConfigInput::new("rootfs", "rootfs", root.path(), true)
                    .with_is_read_only(true)
                    .with_io_engine(DriveIoEngine::Sync),
            ))
            .unwrap_or_else(|error| panic!("{case} root should configure: {error}"));

        let block_layout =
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1));
        let source_serial = SharedSerialOutputBuffer::default();
        let mut session_config = HvfArm64BootSessionConfig::new(
            block_layout,
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            RtcMmioLayout::new(GuestAddress::new(0x4000_1000), MmioRegionId::new(10)),
        )
        .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
            MmioRegionId::new(20),
            GuestAddress::new(0x4000_2000),
            SharedSerialOutput::from(source_serial),
        ));
        if pci_enabled {
            session_config = session_config.with_pci_enabled();
        }
        let mut session = OwnedHvfArm64BootSession::new(&controller, session_config)
            .unwrap_or_else(|error| panic!("{case} signed session should prepare: {error}"));
        let configs =
            CaptureReadyStorageConfigs::new(controller.drive_configs().to_vec(), Vec::new());
        let guard = session
            .quiesce_limiter_retry_wakeups()
            .unwrap_or_else(|error| panic!("{case} retry publishers should quiesce: {error}"));
        let first = session
            .capture_ready_storage_state_at(&configs, &guard, Instant::now())
            .unwrap_or_else(|error| panic!("{case} first root capture should succeed: {error}"));
        let second = session
            .capture_ready_storage_state_at(&configs, &guard, Instant::now())
            .unwrap_or_else(|error| panic!("{case} second root capture should succeed: {error}"));
        let [first_root] = first.block_devices() else {
            panic!("{case} should retain exactly one captured root");
        };
        let [second_root] = second.block_devices() else {
            panic!("{case} recapture should retain exactly one captured root");
        };
        assert!(first.pmem_devices().is_empty());
        assert!(second.pmem_devices().is_empty());

        let first_graph = SnapshotV2DeviceGraph::from_capture_ready_root(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            first_root,
        )
        .unwrap_or_else(|error| panic!("{case} first graph should convert: {error}"));
        let second_graph = SnapshotV2DeviceGraph::from_capture_ready_root(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            second_root,
        )
        .unwrap_or_else(|error| panic!("{case} second graph should convert: {error}"));
        assert_eq!(first_graph, second_graph);
        assert_eq!(first_graph.transport_kind(), expected_transport);
        assert!(first_graph.record_is_root());
        assert_eq!(first_graph.record().config().drive_id(), "rootfs");
        assert_eq!(
            first_graph.record().config().selector(),
            path_text(root.path())
        );
        assert!(first_graph.record().config().is_read_only());
        assert_eq!(
            first_graph.record().config().io_engine(),
            DriveIoEngine::Sync
        );
        assert_eq!(first_graph.record().block().capacity_sectors(), 8);
        assert!(first_graph.record().block().active_queue().is_none());
        assert!(!first_graph.record().virtio().is_activated());
        let [queue] = first_graph.record().virtio().queues() else {
            panic!("{case} should retain one canonical inactive block queue");
        };
        assert_eq!(
            queue.max_size(),
            bangbang_runtime::block::VIRTIO_BLOCK_QUEUE_SIZE
        );
        assert!(!queue.ready());
        assert!(
            first_graph
                .record()
                .virtio()
                .pending_notifications()
                .is_empty()
        );
        assert!(first_graph.record().virtio().interrupt_intents().is_empty());
        match first_graph.record().transport() {
            SnapshotV2DeviceTransport::Mmio(mmio) => {
                assert!(!pci_enabled);
                assert_eq!(mmio.region().id(), block_layout.base_region_id());
                assert_eq!(mmio.region().range().start(), block_layout.base_address());
                assert_eq!(
                    mmio.region().range().size(),
                    bangbang_runtime::virtio_mmio::VIRTIO_MMIO_DEVICE_WINDOW_SIZE
                );
            }
            SnapshotV2DeviceTransport::Pci(pci) => {
                assert!(pci_enabled);
                assert_eq!(pci.origin(), StorageDeviceOrigin::Startup);
                assert_eq!(
                    pci.sbdf(),
                    PciSbdf::new(
                        PCI_SEGMENT_ZERO,
                        PCI_BUS_ZERO,
                        PCI_FIRST_ENDPOINT_DEVICE,
                        PCI_FUNCTION_ZERO,
                    )
                    .expect("root PCI identity should validate")
                );
                assert_eq!(
                    pci.bar_range(),
                    GuestMemoryRange::new(
                        GuestAddress::new(PCI_BAR64_START),
                        VIRTIO_PCI_CAPABILITY_BAR_SIZE,
                    )
                    .expect("root PCI BAR range should validate")
                );
            }
        }

        let first_bytes = first_graph
            .encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION)
            .unwrap_or_else(|error| panic!("{case} graph should encode: {error}"));
        let second_bytes = second_graph
            .encode(NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION)
            .unwrap_or_else(|error| panic!("{case} recaptured graph should encode: {error}"));
        assert_eq!(first_bytes, second_bytes);
        assert_eq!(
            SnapshotV2DeviceGraph::decode(
                NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
                &first_bytes,
            )
            .unwrap_or_else(|error| panic!("{case} immutable graph should decode: {error}")),
            first_graph
        );
        let diagnostics = format!("{first_graph:?} {:?}", first.block_devices());
        assert!(!diagnostics.contains(&path_text(root.path())));
        assert!(!diagnostics.contains("candidate-rootfs"));

        session
            .pause_for_snapshot_v2_capture()
            .unwrap_or_else(|error| panic!("{case} source should pause: {error}"));
        let boot = HvfSnapshotV2BootState::try_new(
            HvfSnapshotV2NativePath::try_new(kernel.path().as_os_str())
                .unwrap_or_else(|error| panic!("{case} kernel path should validate: {error}")),
            None,
            None,
        )
        .unwrap_or_else(|error| panic!("{case} boot metadata should validate: {error}"));
        let capture_input = HvfArm64BootSnapshotV2CaptureInput::new(boot);
        let memory_artifact = TempFile::new_len(&format!("{case}-root-owner-memory"), 0)
            .unwrap_or_else(|error| panic!("{case} memory artifact should create: {error}"));
        let mut memory_writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(memory_artifact.path())
            .unwrap_or_else(|error| panic!("{case} memory artifact should open: {error}"));
        let source_platform = session
            .capture_snapshot_v2_device_graph_platform_with_cancel(
                capture_input.clone(),
                &mut memory_writer,
                |_| false,
            )
            .unwrap_or_else(|error| panic!("{case} exact platform should capture: {error}"));
        let source_state =
            HvfSnapshotV2State::try_new(source_platform.clone(), first_graph.clone())
                .unwrap_or_else(|error| panic!("{case} complete state should validate: {error}"));
        let encoded = encode_hvf_snapshot_v2_state(&source_state)
            .unwrap_or_else(|error| panic!("{case} complete state should encode: {error}"));
        let structural = decode_snapshot_v2_state_with_compatibility_version(
            &encoded,
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
        )
        .unwrap_or_else(|error| panic!("{case} complete state should decode: {error}"));
        let decoded = decode_hvf_snapshot_v2_state(&structural)
            .unwrap_or_else(|error| panic!("{case} typed state should decode: {error}"));
        drop(memory_writer);
        drop(guard);
        session
            .shutdown()
            .unwrap_or_else(|error| panic!("{case} signed session should shut down: {error}"));

        let restored_memory = load_snapshot_v2_memory_file(
            &structural,
            File::open(memory_artifact.path())
                .unwrap_or_else(|error| panic!("{case} memory artifact should reopen: {error}")),
        )
        .unwrap_or_else(|error| panic!("{case} memory should load: {error}"));
        let process = HvfSnapshotV2RootProcessConfig::new(block_layout, pci_enabled);
        let prepared =
            prepare_hvf_snapshot_v2_root_plan(decoded, restored_memory, process, Instant::now())
                .unwrap_or_else(|error| panic!("{case} root plan should prepare: {error}"));
        let (platform, memory, root_plan, resources) = prepared.into_parts();
        let (backing, _identity) = BlockFileBacking::open_snapshot_read_only(root.path())
            .unwrap_or_else(|error| panic!("{case} root backing should reopen: {error}"));
        let prepared_root = root_plan
            .prepare_backing(backing)
            .unwrap_or_else(|error| panic!("{case} root backing should validate: {error}"));
        let restored_serial = SharedSerialOutputBuffer::default();
        let shell =
            HvfSnapshotV2DefaultProcessShell::new(SharedSerialOutput::from(restored_serial));
        let mut restored = OwnedHvfArm64BootSession::restore_snapshot_v2_root(
            platform,
            memory,
            shell,
            prepared_root,
            resources,
        )
        .unwrap_or_else(|error| panic!("{case} complete root owner should restore: {error:?}"));
        assert_eq!(restored.uses_pci_data_devices(), pci_enabled);
        assert!(restored.restored_snapshot_v2_memory_binding().is_some());
        assert_eq!(
            restored.restored_snapshot_v2_machine(),
            Some(source_platform.machine())
        );
        let restored_topology = restored
            .capture_stable_paused_vcpu_topology()
            .unwrap_or_else(|error| panic!("{case} paused topology should recapture: {error}"));
        assert_eq!(restored_topology, *source_platform.topology());

        let restored_guard = restored
            .quiesce_limiter_retry_wakeups()
            .unwrap_or_else(|error| {
                panic!("{case} restored retry publishers should quiesce: {error}")
            });
        let restored_storage = restored
            .capture_ready_storage_state_at(&configs, &restored_guard, Instant::now())
            .unwrap_or_else(|error| panic!("{case} restored root should recapture: {error}"));
        let [restored_root] = restored_storage.block_devices() else {
            panic!("{case} restored owner should retain exactly one root");
        };
        let restored_graph = SnapshotV2DeviceGraph::from_capture_ready_root(
            NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION,
            restored_root,
        )
        .unwrap_or_else(|error| panic!("{case} restored graph should convert: {error}"));
        assert_eq!(restored_graph, first_graph);

        let recaptured_memory = TempFile::new_len(&format!("{case}-root-owner-recapture"), 0)
            .unwrap_or_else(|error| panic!("{case} recapture artifact should create: {error}"));
        let mut recaptured_writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(recaptured_memory.path())
            .unwrap_or_else(|error| panic!("{case} recapture artifact should open: {error}"));
        let recaptured_platform = restored
            .capture_snapshot_v2_device_graph_platform_with_cancel(
                capture_input,
                &mut recaptured_writer,
                |_| false,
            )
            .unwrap_or_else(|error| panic!("{case} restored platform should recapture: {error}"));
        assert_native_v2_platform_recapture_equivalent(&source_platform, &recaptured_platform);

        drop(restored_guard);
        restored
            .shutdown()
            .unwrap_or_else(|error| panic!("{case} restored owner should shut down: {error}"));
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_network_traverses_signed_mmio_and_pci_owners() {
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootNetworkCaptureConfig, HvfArm64BootNetworkCaptureError,
        HvfArm64BootNetworkDeviceOrigin, HvfArm64BootNetworkTransportCaptureState,
        HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::{
        NetworkDeviceProfile, NetworkInterfaceConfigInput, NetworkMmioLayout,
        NetworkRateLimiterConfig, NetworkTokenBucketConfig, PreparedNetworkDevice,
        VirtioNetworkRateLimiterCaptureState,
    };
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("capture-ready-network-kernel", &image)
        .expect("network capture kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("network capture boot source should configure");
    let limiter = NetworkRateLimiterConfig::new(
        Some(NetworkTokenBucketConfig::new(4096, Some(8192), 100)),
        Some(NetworkTokenBucketConfig::new(64, None, 100)),
    );
    controller
        .handle_action(VmmAction::PutNetworkInterface(
            NetworkInterfaceConfigInput::new("eth0", "eth0", "private-network-backend")
                .with_guest_mac("02:00:00:00:00:41")
                .with_mtu(1400)
                .with_rx_rate_limiter(limiter)
                .with_tx_rate_limiter(limiter),
        ))
        .expect("startup capture network should configure");
    let startup_configs = controller.network_interface_configs().to_vec();
    let capture_configs = |configs: &[bangbang_runtime::network::NetworkInterfaceConfig]| {
        configs
            .iter()
            .map(|config| {
                HvfArm64BootNetworkCaptureConfig::new(
                    config.clone(),
                    NetworkDeviceProfile::from_config(config),
                    None,
                    None,
                    false,
                )
            })
            .collect::<Vec<_>>()
    };
    let base_session_config = || {
        HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            test_rtc_mmio_layout(),
        )
    };

    let mut mmio_session = OwnedHvfArm64BootSession::new(&controller, base_session_config())
        .expect("signed MMIO network session should prepare");
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO network retry publisher should quiesce");
    let now = Instant::now();
    let mmio_first = mmio_session
        .capture_ready_network_state_at(&capture_configs(&startup_configs), &mmio_guard, now)
        .expect("signed MMIO network should become capture-ready");
    let mmio_second = mmio_session
        .capture_ready_network_state_at(&capture_configs(&startup_configs), &mmio_guard, now)
        .expect("signed MMIO network capture should repeat");
    assert_eq!(mmio_first, mmio_second);
    assert_eq!(mmio_first.interfaces().len(), 1);
    let HvfArm64BootNetworkTransportCaptureState::Mmio {
        region,
        interrupt_line,
        state,
    } = mmio_first.interfaces()[0].transport()
    else {
        panic!("MMIO network should retain MMIO ownership");
    };
    assert_eq!(region.range().start(), GuestAddress::new(0x6000_0000));
    assert!(interrupt_line.raw_value() > 0);
    assert!(state.device().active_rx_queue().is_none());
    assert!(state.device().active_tx_queue().is_none());
    assert!(state.device().rx_rate_limiter().is_configured());
    assert!(state.device().tx_rate_limiter().is_configured());
    assert!(!state.transport().is_device_activated());
    let mmio_device = state.device().clone();
    let debug = format!("{mmio_first:?}");
    assert!(!debug.contains("private-network-backend"));
    assert!(!debug.contains("02:00:00:00:00:41"));
    assert!(matches!(
        mmio_session.capture_ready_network_state_at(&[], &mmio_guard, now),
        Err(HvfArm64BootNetworkCaptureError::InventoryMismatch)
    ));
    drop(mmio_guard);
    mmio_session
        .shutdown()
        .expect("signed MMIO network session should shut down");

    let mut pci_session =
        OwnedHvfArm64BootSession::new(&controller, base_session_config().with_pci_enabled())
            .expect("signed PCI network session should prepare");
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI network retry publisher should quiesce");
    let pci_now = Instant::now();
    let pci_capture_configs = capture_configs(&startup_configs);
    let pci_startup = pci_session
        .capture_ready_network_state_at(&pci_capture_configs, &pci_guard, pci_now)
        .expect("signed startup PCI network should become capture-ready");
    let pci_repeated = pci_session
        .capture_ready_network_state_at(&pci_capture_configs, &pci_guard, pci_now)
        .expect("signed startup PCI network capture should repeat");
    assert_eq!(pci_startup, pci_repeated);
    let HvfArm64BootNetworkTransportCaptureState::Pci {
        origin,
        sbdf,
        bar_range,
        state,
    } = pci_startup.interfaces()[0].transport()
    else {
        panic!("PCI network should retain PCI ownership");
    };
    assert_eq!(*origin, HvfArm64BootNetworkDeviceOrigin::Startup);
    assert!(sbdf.device() > 0);
    assert_eq!(
        bar_range.size(),
        bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE
    );
    let pci_device = state.device();
    assert_eq!(pci_device.profile(), mmio_device.profile());
    assert_eq!(
        pci_device.available_features(),
        mmio_device.available_features()
    );
    assert_eq!(
        pci_device.negotiated_features(),
        mmio_device.negotiated_features()
    );
    assert_eq!(pci_device.active_rx_queue(), mmio_device.active_rx_queue());
    assert_eq!(pci_device.active_tx_queue(), mmio_device.active_tx_queue());
    assert_eq!(
        pci_device.source_rx_cache_normalized(),
        mmio_device.source_rx_cache_normalized()
    );
    assert_eq!(
        pci_device.source_rx_retry_normalized(),
        mmio_device.source_rx_retry_normalized()
    );
    assert_eq!(pci_device.tx_retry(), mmio_device.tx_retry());
    let limiter_shape = |limiter: VirtioNetworkRateLimiterCaptureState| {
        (
            limiter
                .bandwidth()
                .map(|bucket| (bucket.config(), bucket.budget(), bucket.one_time_burst())),
            limiter
                .ops()
                .map(|bucket| (bucket.config(), bucket.budget(), bucket.one_time_burst())),
        )
    };
    assert_eq!(
        limiter_shape(pci_device.rx_rate_limiter()),
        limiter_shape(mmio_device.rx_rate_limiter())
    );
    assert_eq!(
        limiter_shape(pci_device.tx_rate_limiter()),
        limiter_shape(mmio_device.tx_rate_limiter())
    );
    drop(pci_guard);

    controller
        .handle_action(VmmAction::PutNetworkInterface(
            NetworkInterfaceConfigInput::new("eth1", "eth1", "runtime-private-backend")
                .with_guest_mac("02:00:00:00:00:42"),
        ))
        .expect("runtime capture network should join controller inventory");
    let runtime_config = controller.network_interface_configs()[1].clone();
    pci_session
        .insert_runtime_network_device(PreparedNetworkDevice::from_config(&runtime_config))
        .expect("runtime PCI network should publish");
    let all_configs = controller.network_interface_configs().to_vec();
    let all_capture_configs = capture_configs(&all_configs);
    let runtime_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("runtime PCI network retry publisher should quiesce");
    let duplicate_capture_configs = vec![
        all_capture_configs[0].clone(),
        all_capture_configs[0].clone(),
    ];
    assert!(matches!(
        pci_session.capture_ready_network_state_at(
            &duplicate_capture_configs,
            &runtime_guard,
            Instant::now(),
        ),
        Err(HvfArm64BootNetworkCaptureError::InventoryMismatch)
    ));
    let runtime_capture = pci_session
        .capture_ready_network_state_at(&all_capture_configs, &runtime_guard, Instant::now())
        .expect("startup/runtime PCI network inventory should capture");
    assert_eq!(runtime_capture.interfaces().len(), 2);
    let origins = runtime_capture
        .interfaces()
        .iter()
        .map(|interface| match interface.transport() {
            HvfArm64BootNetworkTransportCaptureState::Pci { origin, .. } => *origin,
            HvfArm64BootNetworkTransportCaptureState::Mmio { .. } => {
                panic!("PCI inventory must not capture an MMIO owner")
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(
        origins,
        [
            HvfArm64BootNetworkDeviceOrigin::Startup,
            HvfArm64BootNetworkDeviceOrigin::Runtime,
        ]
    );
    drop(runtime_guard);

    let removed = pci_session
        .prepare_runtime_network_device_removal("eth1")
        .expect("runtime PCI network removal should prepare");
    let missing_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("partially removed inventory retry publisher should quiesce");
    assert!(matches!(
        pci_session.capture_ready_network_state_at(
            &all_capture_configs,
            &missing_guard,
            Instant::now(),
        ),
        Err(HvfArm64BootNetworkCaptureError::PciCapture)
    ));
    drop(missing_guard);
    pci_session
        .rollback_runtime_network_device_removal(removed)
        .expect("failed capture topology should remain rollback-safe");

    let removed = pci_session
        .prepare_runtime_network_device_removal("eth1")
        .expect("runtime PCI network replacement removal should prepare");
    pci_session
        .commit_runtime_network_device_removal(removed)
        .expect("runtime PCI network replacement removal should commit");
    pci_session
        .insert_runtime_network_device(PreparedNetworkDevice::from_config(&runtime_config))
        .expect("same-ID runtime PCI network should republish");
    let replacement_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("replacement PCI network retry publisher should quiesce");
    let replacement = pci_session
        .capture_ready_network_state_at(&all_capture_configs, &replacement_guard, Instant::now())
        .expect("same-ID replacement PCI network should capture");
    assert!(matches!(
        replacement.interfaces()[1].transport(),
        HvfArm64BootNetworkTransportCaptureState::Pci {
            origin: HvfArm64BootNetworkDeviceOrigin::Runtime,
            ..
        }
    ));
    drop(replacement_guard);
    pci_session
        .shutdown()
        .expect("signed PCI network session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_balloon_traverses_signed_mmio_and_pci_owners() {
    use std::io::Cursor;

    use bangbang_hvf::{
        HvfArm64BootBalloonCaptureError, HvfArm64BootBalloonDeviceConfig,
        HvfArm64BootBalloonTransportState, HvfArm64BootSessionConfig,
        HvfArm64BootSnapshotV2CaptureInput, HvfSnapshotV2BootState, HvfSnapshotV2NativePath,
        OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::balloon::{BalloonConfigInput, BalloonMmioLayout, available_features};
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::snapshot_balloon_v2_9::NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("capture-ready-balloon-kernel", &image)
        .expect("balloon capture kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("balloon capture boot source should configure");
    controller
        .handle_action(VmmAction::PutBalloon(
            BalloonConfigInput::new(8, true)
                .with_stats_polling_interval_s(1)
                .with_free_page_hinting(true)
                .with_free_page_reporting(true),
        ))
        .expect("balloon capture device should configure");
    let balloon_config = controller
        .balloon_config()
        .expect("balloon config should exist");
    let balloon_device = HvfArm64BootBalloonDeviceConfig::new(BalloonMmioLayout::new(
        GuestAddress::new(0x4000_8000),
        MmioRegionId::new(4000),
    ));
    let base_session_config = || {
        HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            test_rtc_mmio_layout(),
        )
        .with_balloon_device(balloon_device)
    };

    let mut mmio_session = OwnedHvfArm64BootSession::new(&controller, base_session_config())
        .expect("signed MMIO balloon session should prepare");
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO auxiliary publishers should quiesce");
    let mmio_first = mmio_session
        .capture_ready_balloon_state(Some(balloon_config), &mmio_guard)
        .expect("signed MMIO balloon should become capture-ready")
        .expect("configured MMIO balloon should be captured");
    let mmio_second = mmio_session
        .capture_ready_balloon_state(Some(balloon_config), &mmio_guard)
        .expect("signed MMIO balloon should support repeated detached capture")
        .expect("configured MMIO balloon should remain captured");
    assert_eq!(mmio_first.config(), balloon_config);
    let HvfArm64BootBalloonTransportState::Mmio { state, .. } = mmio_first.transport() else {
        panic!("MMIO balloon should retain MMIO ownership");
    };
    assert_eq!(
        state.device().available_features(),
        available_features(balloon_config)
    );
    assert!(state.device().active_queues().is_none());
    let mmio_snapshot = mmio_first
        .try_to_snapshot_v2()
        .expect("signed MMIO balloon capture should convert to exact 2.9");
    assert_eq!(mmio_snapshot.config(), balloon_config);
    assert_eq!(
        mmio_snapshot.virtio().queues().len(),
        state.device().queue_layout().queue_count()
    );
    assert_eq!(mmio_first, mmio_second);
    assert!(!format!("{mmio_first:?}").contains("40008000"));
    assert!(matches!(
        mmio_session.capture_ready_balloon_state(None, &mmio_guard),
        Err(HvfArm64BootBalloonCaptureError::OwnershipMismatch {
            configured: false,
            mmio_owner: true,
            pci_owner: false,
        })
    ));
    drop(mmio_guard);
    mmio_session
        .shutdown()
        .expect("signed MMIO balloon session should shut down");

    let mut pci_session =
        OwnedHvfArm64BootSession::new(&controller, base_session_config().with_pci_enabled())
            .expect("signed PCI balloon session should prepare");
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI auxiliary publishers should quiesce");
    let pci = pci_session
        .capture_ready_balloon_state(Some(balloon_config), &pci_guard)
        .expect("signed PCI balloon should become capture-ready")
        .expect("configured PCI balloon should be captured");
    let HvfArm64BootBalloonTransportState::Pci {
        sbdf,
        bar_range,
        state,
    } = pci.transport()
    else {
        panic!("PCI balloon should retain PCI ownership");
    };
    assert!(sbdf.device() > 0);
    assert_eq!(
        bar_range.size(),
        bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE
    );
    assert_eq!(
        state.device().available_features(),
        available_features(balloon_config)
    );
    assert!(state.device().active_queues().is_none());
    assert!(
        !state.transport().requires_device_config_write_status(),
        "PCI balloon should retain the infallible device-config write contract"
    );
    assert_eq!(
        state.transport().msix_vector_count(),
        state.device().queue_layout().queue_count() + 1,
        "PCI balloon should allocate one MSI-X vector per queue plus configuration"
    );
    let pci_snapshot = pci
        .try_to_snapshot_v2()
        .expect("signed PCI balloon capture should convert to exact 2.9");
    assert_eq!(pci_snapshot.config(), balloon_config);
    assert_eq!(
        pci_snapshot.virtio().queues().len(),
        state.device().queue_layout().queue_count()
    );
    assert!(!format!("{pci:?}").contains("40008000"));
    drop(pci_guard);
    pci_session
        .pause_for_snapshot_v2_capture()
        .expect("signed PCI balloon source should pause");
    let boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_new(kernel.path().as_os_str())
            .expect("balloon kernel path should validate"),
        None,
        None,
    )
    .expect("balloon boot metadata should validate");
    let mut memory_writer = Cursor::new(Vec::new());
    let platform = pci_session
        .capture_snapshot_v2_balloon_platform_with_cancel(
            HvfArm64BootSnapshotV2CaptureInput::new(boot),
            &mut memory_writer,
            |_| false,
        )
        .expect("signed exact-2.9 balloon platform should capture");
    assert_eq!(
        platform.memory().version(),
        NATIVE_V2_BALLOON_STATE_COMPATIBILITY_VERSION
    );
    pci_session
        .shutdown()
        .expect("signed PCI balloon session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_QUEUE_SIZE: u16 = 8;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_DESCRIPTOR_TABLE: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8040_0000);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_AVAILABLE_RING: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8041_0000);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_USED_RING: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8042_0000);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_FIRST_DATA: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8043_0000);
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const ENTROPY_CAPTURE_SECOND_DATA: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8044_0000);

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_entropy_capture_mmio(
    dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher,
    address: bangbang_runtime::memory::GuestAddress,
    data: &[u8],
) {
    use bangbang_runtime::mmio::{MmioAccessBytes, MmioDispatchOutcome, MmioOperation};

    let access = dispatcher
        .lookup(
            address,
            u64::try_from(data.len()).expect("entropy MMIO write length should fit u64"),
        )
        .expect("entropy MMIO write should resolve");
    let outcome = dispatcher
        .dispatch(
            MmioOperation::write(
                access,
                MmioAccessBytes::new(data).expect("entropy MMIO bytes should validate"),
            )
            .expect("entropy MMIO operation should validate"),
        )
        .expect("entropy MMIO write should dispatch");
    assert!(matches!(outcome, MmioDispatchOutcome::Write));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_entropy_capture_queue(memory: &mut bangbang_runtime::memory::GuestMemory) {
    use bangbang_runtime::virtio_queue::{VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE};

    for (index, data_address) in [ENTROPY_CAPTURE_FIRST_DATA, ENTROPY_CAPTURE_SECOND_DATA]
        .into_iter()
        .enumerate()
    {
        let descriptor_address = ENTROPY_CAPTURE_DESCRIPTOR_TABLE
            .checked_add(
                u64::try_from(index).expect("descriptor index should fit u64")
                    * u64::try_from(VIRTQUEUE_DESCRIPTOR_SIZE)
                        .expect("descriptor size should fit u64"),
            )
            .expect("descriptor address should not overflow");
        memory
            .write_slice(&data_address.raw_value().to_le_bytes(), descriptor_address)
            .expect("entropy descriptor address should write");
        memory
            .write_slice(
                &4_u32.to_le_bytes(),
                descriptor_address.checked_add(8).unwrap(),
            )
            .expect("entropy descriptor length should write");
        memory
            .write_slice(
                &VIRTQUEUE_DESC_F_WRITE.to_le_bytes(),
                descriptor_address.checked_add(12).unwrap(),
            )
            .expect("entropy descriptor flags should write");
        memory
            .write_slice(
                &0_u16.to_le_bytes(),
                descriptor_address.checked_add(14).unwrap(),
            )
            .expect("entropy descriptor next index should write");
    }
    memory
        .write_slice(&0_u16.to_le_bytes(), ENTROPY_CAPTURE_AVAILABLE_RING)
        .expect("entropy available flags should write");
    memory
        .write_slice(
            &2_u16.to_le_bytes(),
            ENTROPY_CAPTURE_AVAILABLE_RING.checked_add(2).unwrap(),
        )
        .expect("entropy available index should write");
    memory
        .write_slice(
            &0_u16.to_le_bytes(),
            ENTROPY_CAPTURE_AVAILABLE_RING.checked_add(4).unwrap(),
        )
        .expect("first entropy available head should write");
    memory
        .write_slice(
            &1_u16.to_le_bytes(),
            ENTROPY_CAPTURE_AVAILABLE_RING.checked_add(6).unwrap(),
        )
        .expect("second entropy available head should write");
    memory
        .write_slice(&[0; 4], ENTROPY_CAPTURE_USED_RING)
        .expect("entropy used ring header should reset");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn activate_and_notify_entropy_capture_queue(
    session: &mut bangbang_hvf::OwnedHvfArm64BootSession,
    transport_base: bangbang_runtime::memory::GuestAddress,
    pci: bool,
) -> std::time::Duration {
    use bangbang_runtime::entropy::VirtioRngOsEntropySource;
    use bangbang_runtime::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK, VirtioMmioRegister,
    };
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_NOTIFICATION_OFFSET;

    let dispatcher = session.mmio_dispatcher();
    write_entropy_capture_queue(
        session
            .guest_memory_mut()
            .expect("signed entropy guest memory should remain mapped"),
    );
    let mut dispatcher = dispatcher
        .lock()
        .expect("signed entropy MMIO dispatcher should not be poisoned");
    let write =
        |dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher, offset: u64, data: &[u8]| {
            write_entropy_capture_mmio(
                dispatcher,
                transport_base
                    .checked_add(offset)
                    .expect("entropy transport address should not overflow"),
                data,
            );
        };
    let features_ok = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    let driver_ok = features_ok | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    if pci {
        write(
            &mut dispatcher,
            0x14,
            &[u8::try_from(VIRTIO_DEVICE_STATUS_ACKNOWLEDGE).unwrap()],
        );
        write(
            &mut dispatcher,
            0x14,
            &[
                u8::try_from(VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER)
                    .unwrap(),
            ],
        );
        write(&mut dispatcher, 0x08, &1_u32.to_le_bytes());
        write(&mut dispatcher, 0x0c, &1_u32.to_le_bytes());
        write(&mut dispatcher, 0x14, &[u8::try_from(features_ok).unwrap()]);
        write(&mut dispatcher, 0x16, &0_u16.to_le_bytes());
        write(
            &mut dispatcher,
            0x18,
            &ENTROPY_CAPTURE_QUEUE_SIZE.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            0x20,
            &u32::try_from(ENTROPY_CAPTURE_DESCRIPTOR_TABLE.raw_value())
                .unwrap()
                .to_le_bytes(),
        );
        write(
            &mut dispatcher,
            0x28,
            &u32::try_from(ENTROPY_CAPTURE_AVAILABLE_RING.raw_value())
                .unwrap()
                .to_le_bytes(),
        );
        write(
            &mut dispatcher,
            0x30,
            &u32::try_from(ENTROPY_CAPTURE_USED_RING.raw_value())
                .unwrap()
                .to_le_bytes(),
        );
        write(&mut dispatcher, 0x1c, &1_u16.to_le_bytes());
        write(&mut dispatcher, 0x14, &[u8::try_from(driver_ok).unwrap()]);
        write(
            &mut dispatcher,
            VIRTIO_PCI_NOTIFICATION_OFFSET,
            &0_u16.to_le_bytes(),
        );
    } else {
        for status in [
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        ] {
            write(
                &mut dispatcher,
                VirtioMmioRegister::Status.offset(),
                &status.to_le_bytes(),
            );
        }
        write(
            &mut dispatcher,
            VirtioMmioRegister::DriverFeaturesSel.offset(),
            &1_u32.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            VirtioMmioRegister::DriverFeatures.offset(),
            &1_u32.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            VirtioMmioRegister::Status.offset(),
            &features_ok.to_le_bytes(),
        );
        for (register, value) in [
            (
                VirtioMmioRegister::QueueNum,
                u32::from(ENTROPY_CAPTURE_QUEUE_SIZE),
            ),
            (
                VirtioMmioRegister::QueueDescLow,
                u32::try_from(ENTROPY_CAPTURE_DESCRIPTOR_TABLE.raw_value()).unwrap(),
            ),
            (
                VirtioMmioRegister::QueueDriverLow,
                u32::try_from(ENTROPY_CAPTURE_AVAILABLE_RING.raw_value()).unwrap(),
            ),
            (
                VirtioMmioRegister::QueueDeviceLow,
                u32::try_from(ENTROPY_CAPTURE_USED_RING.raw_value()).unwrap(),
            ),
            (VirtioMmioRegister::QueueReady, 1),
        ] {
            write(&mut dispatcher, register.offset(), &value.to_le_bytes());
        }
        write(
            &mut dispatcher,
            VirtioMmioRegister::Status.offset(),
            &driver_ok.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            VirtioMmioRegister::QueueNotify.offset(),
            &0_u32.to_le_bytes(),
        );
    }
    drop(dispatcher);

    let mut source = VirtioRngOsEntropySource::new();
    session
        .dispatch_entropy_queue_notifications_and_schedule_retry_wakeup(&mut source)
        .expect("signed entropy owner should dispatch and schedule retry")
        .rate_limiter_retry_after()
        .expect("second entropy descriptor should be throttled")
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VSOCK_CAPTURE_QUEUE_SIZE: u16 = 256;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VSOCK_CAPTURE_QUEUE_RINGS: [(
    bangbang_runtime::memory::GuestAddress,
    bangbang_runtime::memory::GuestAddress,
    bangbang_runtime::memory::GuestAddress,
); 3] = [
    (
        bangbang_runtime::memory::GuestAddress::new(0x8060_0000),
        bangbang_runtime::memory::GuestAddress::new(0x8061_0000),
        bangbang_runtime::memory::GuestAddress::new(0x8062_0000),
    ),
    (
        bangbang_runtime::memory::GuestAddress::new(0x8063_0000),
        bangbang_runtime::memory::GuestAddress::new(0x8064_0000),
        bangbang_runtime::memory::GuestAddress::new(0x8065_0000),
    ),
    (
        bangbang_runtime::memory::GuestAddress::new(0x8066_0000),
        bangbang_runtime::memory::GuestAddress::new(0x8067_0000),
        bangbang_runtime::memory::GuestAddress::new(0x8068_0000),
    ),
];
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const VSOCK_CAPTURE_EVENT_PAYLOAD: bangbang_runtime::memory::GuestAddress =
    bangbang_runtime::memory::GuestAddress::new(0x8069_0000);

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_vsock_capture_mmio(
    dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher,
    address: bangbang_runtime::memory::GuestAddress,
    data: &[u8],
) {
    use bangbang_runtime::mmio::{MmioAccessBytes, MmioDispatchOutcome, MmioOperation};

    let access = dispatcher
        .lookup(
            address,
            u64::try_from(data.len()).expect("vsock MMIO write length should fit u64"),
        )
        .expect("vsock MMIO write should resolve");
    let outcome = dispatcher
        .dispatch(
            MmioOperation::write(
                access,
                MmioAccessBytes::new(data).expect("vsock MMIO bytes should validate"),
            )
            .expect("vsock MMIO operation should validate"),
        )
        .expect("vsock MMIO write should dispatch");
    assert!(matches!(outcome, MmioDispatchOutcome::Write));
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_vsock_capture_event(
    memory: &mut bangbang_runtime::memory::GuestMemory,
    descriptor_index: u16,
    available_index: u16,
) {
    use bangbang_runtime::virtio_queue::{VIRTQUEUE_DESC_F_WRITE, VIRTQUEUE_DESCRIPTOR_SIZE};

    let (descriptor_table, available_ring, _) = VSOCK_CAPTURE_QUEUE_RINGS[2];
    let descriptor = descriptor_table
        .checked_add(u64::from(descriptor_index) * VIRTQUEUE_DESCRIPTOR_SIZE as u64)
        .expect("vsock event descriptor address should fit");
    let payload = VSOCK_CAPTURE_EVENT_PAYLOAD
        .checked_add(u64::from(descriptor_index) * 8)
        .expect("vsock event payload address should fit");
    memory
        .write_slice(&payload.raw_value().to_le_bytes(), descriptor)
        .expect("vsock event descriptor address should write");
    memory
        .write_slice(&4_u32.to_le_bytes(), descriptor.checked_add(8).unwrap())
        .expect("vsock event descriptor length should write");
    memory
        .write_slice(
            &VIRTQUEUE_DESC_F_WRITE.to_le_bytes(),
            descriptor.checked_add(12).unwrap(),
        )
        .expect("vsock event descriptor flags should write");
    memory
        .write_slice(&0_u16.to_le_bytes(), descriptor.checked_add(14).unwrap())
        .expect("vsock event descriptor next index should write");
    memory
        .write_slice(&[0xff; 4], payload)
        .expect("vsock event payload should initialize");
    let available_head = available_ring
        .checked_add(4 + u64::from(available_index - 1) * 2)
        .expect("vsock available head address should fit");
    memory
        .write_slice(&descriptor_index.to_le_bytes(), available_head)
        .expect("vsock event available head should write");
    memory
        .write_slice(
            &available_index.to_le_bytes(),
            available_ring.checked_add(2).unwrap(),
        )
        .expect("vsock event available index should write");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn activate_vsock_capture_queues(
    session: &mut bangbang_hvf::OwnedHvfArm64BootSession,
    transport_base: bangbang_runtime::memory::GuestAddress,
    pci: bool,
) {
    use bangbang_runtime::virtio_mmio::{
        VIRTIO_DEVICE_STATUS_ACKNOWLEDGE, VIRTIO_DEVICE_STATUS_DRIVER,
        VIRTIO_DEVICE_STATUS_DRIVER_OK, VIRTIO_DEVICE_STATUS_FEATURES_OK, VirtioMmioRegister,
    };

    let dispatcher = session.mmio_dispatcher();
    let memory = session
        .guest_memory_mut()
        .expect("signed vsock guest memory should remain mapped");
    for (_, available_ring, used_ring) in VSOCK_CAPTURE_QUEUE_RINGS {
        memory
            .write_slice(&[0; 4], available_ring)
            .expect("vsock available ring header should initialize");
        memory
            .write_slice(&[0; 4], used_ring)
            .expect("vsock used ring header should initialize");
    }
    write_vsock_capture_event(memory, 0, 1);

    let mut dispatcher = dispatcher
        .lock()
        .expect("signed vsock MMIO dispatcher should not be poisoned");
    let write =
        |dispatcher: &mut bangbang_runtime::mmio::MmioDispatcher, offset: u64, data: &[u8]| {
            write_vsock_capture_mmio(
                dispatcher,
                transport_base
                    .checked_add(offset)
                    .expect("vsock transport address should not overflow"),
                data,
            );
        };
    let features_ok = VIRTIO_DEVICE_STATUS_ACKNOWLEDGE
        | VIRTIO_DEVICE_STATUS_DRIVER
        | VIRTIO_DEVICE_STATUS_FEATURES_OK;
    let driver_ok = features_ok | VIRTIO_DEVICE_STATUS_DRIVER_OK;
    if pci {
        write(
            &mut dispatcher,
            0x14,
            &[u8::try_from(VIRTIO_DEVICE_STATUS_ACKNOWLEDGE).unwrap()],
        );
        write(
            &mut dispatcher,
            0x14,
            &[
                u8::try_from(VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER)
                    .unwrap(),
            ],
        );
        write(&mut dispatcher, 0x08, &1_u32.to_le_bytes());
        write(&mut dispatcher, 0x0c, &1_u32.to_le_bytes());
        write(&mut dispatcher, 0x14, &[u8::try_from(features_ok).unwrap()]);
        for (queue_index, (descriptor, available, used)) in
            VSOCK_CAPTURE_QUEUE_RINGS.into_iter().enumerate()
        {
            write(
                &mut dispatcher,
                0x16,
                &u16::try_from(queue_index).unwrap().to_le_bytes(),
            );
            write(
                &mut dispatcher,
                0x18,
                &VSOCK_CAPTURE_QUEUE_SIZE.to_le_bytes(),
            );
            write(
                &mut dispatcher,
                0x20,
                &u32::try_from(descriptor.raw_value()).unwrap().to_le_bytes(),
            );
            write(
                &mut dispatcher,
                0x28,
                &u32::try_from(available.raw_value()).unwrap().to_le_bytes(),
            );
            write(
                &mut dispatcher,
                0x30,
                &u32::try_from(used.raw_value()).unwrap().to_le_bytes(),
            );
            write(&mut dispatcher, 0x1c, &1_u16.to_le_bytes());
        }
        write(&mut dispatcher, 0x14, &[u8::try_from(driver_ok).unwrap()]);
    } else {
        for status in [
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE,
            VIRTIO_DEVICE_STATUS_ACKNOWLEDGE | VIRTIO_DEVICE_STATUS_DRIVER,
        ] {
            write(
                &mut dispatcher,
                VirtioMmioRegister::Status.offset(),
                &status.to_le_bytes(),
            );
        }
        write(
            &mut dispatcher,
            VirtioMmioRegister::DriverFeaturesSel.offset(),
            &1_u32.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            VirtioMmioRegister::DriverFeatures.offset(),
            &1_u32.to_le_bytes(),
        );
        write(
            &mut dispatcher,
            VirtioMmioRegister::Status.offset(),
            &features_ok.to_le_bytes(),
        );
        for (queue_index, (descriptor, available, used)) in
            VSOCK_CAPTURE_QUEUE_RINGS.into_iter().enumerate()
        {
            write(
                &mut dispatcher,
                VirtioMmioRegister::QueueSel.offset(),
                &u32::try_from(queue_index).unwrap().to_le_bytes(),
            );
            for (register, value) in [
                (
                    VirtioMmioRegister::QueueNum,
                    u32::from(VSOCK_CAPTURE_QUEUE_SIZE),
                ),
                (
                    VirtioMmioRegister::QueueDescLow,
                    u32::try_from(descriptor.raw_value()).unwrap(),
                ),
                (
                    VirtioMmioRegister::QueueDriverLow,
                    u32::try_from(available.raw_value()).unwrap(),
                ),
                (
                    VirtioMmioRegister::QueueDeviceLow,
                    u32::try_from(used.raw_value()).unwrap(),
                ),
                (VirtioMmioRegister::QueueReady, 1),
            ] {
                write(&mut dispatcher, register.offset(), &value.to_le_bytes());
            }
        }
        write(
            &mut dispatcher,
            VirtioMmioRegister::Status.offset(),
            &driver_ok.to_le_bytes(),
        );
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn notify_vsock_capture_event_queue(
    session: &mut bangbang_hvf::OwnedHvfArm64BootSession,
    transport_base: bangbang_runtime::memory::GuestAddress,
    pci: bool,
) {
    use bangbang_runtime::virtio_mmio::VirtioMmioRegister;
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_NOTIFICATION_OFFSET;
    use bangbang_runtime::vsock::VIRTIO_VSOCK_EVENT_QUEUE_INDEX;

    let dispatcher = session.mmio_dispatcher();
    let mut dispatcher = dispatcher
        .lock()
        .expect("signed vsock MMIO dispatcher should not be poisoned");
    let (offset, bytes) = if pci {
        (
            VIRTIO_PCI_NOTIFICATION_OFFSET,
            u16::try_from(VIRTIO_VSOCK_EVENT_QUEUE_INDEX)
                .unwrap()
                .to_le_bytes()
                .to_vec(),
        )
    } else {
        (
            VirtioMmioRegister::QueueNotify.offset(),
            u32::try_from(VIRTIO_VSOCK_EVENT_QUEUE_INDEX)
                .unwrap()
                .to_le_bytes()
                .to_vec(),
        )
    };
    write_vsock_capture_mmio(
        &mut dispatcher,
        transport_base.checked_add(offset).unwrap(),
        &bytes,
    );
    drop(dispatcher);
    session
        .dispatch_vsock_queue_notifications_and_signal_interrupts()
        .expect("signed vsock event acknowledgement should dispatch");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_vsock_resets_signed_mmio_and_pci_owners() {
    use bangbang_hvf::{
        HvfArm64BootSessionConfig, HvfArm64BootVsockCaptureDisposition,
        HvfArm64BootVsockCaptureStage, HvfArm64BootVsockTransportState, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE;
    use bangbang_runtime::vsock::{
        VIRTIO_VSOCK_EVENT_TRANSPORT_RESET, VirtioVsockTransportResetAttempt, VsockConfigInput,
        VsockMmioLayout,
    };

    // The wrapper already runs this direct-listener case as a signed binary.
    // Its App Sandbox replay intentionally has no network server entitlement;
    // supplied-listener containment is covered by the production process tests.
    if is_app_sandbox_hvf_lifecycle_replay() {
        return;
    }
    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("capture-ready-vsock-kernel", &image)
        .expect("vsock capture kernel should create");
    let socket_id = NEXT_HVF_TEST_FILE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let socket_path =
        std::env::temp_dir().join(format!("bb-vsock-{}-{socket_id}.sock", std::process::id()));
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("vsock capture boot source should configure");
    controller
        .handle_action(VmmAction::PutVsock(VsockConfigInput::new(
            42,
            path_text(&socket_path),
        )))
        .expect("vsock capture device should configure");
    let vsock_config = controller
        .vsock_config()
        .cloned()
        .expect("vsock capture config should exist");
    let base_session_config = || {
        HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            test_rtc_mmio_layout(),
        )
    };

    let mut mmio_session = OwnedHvfArm64BootSession::new(&controller, base_session_config())
        .expect("signed MMIO vsock session should prepare");
    let mmio_metrics = mmio_session.shared_vsock_device_metrics();
    let mmio_idle_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO vsock auxiliary work should quiesce");
    let mmio_idle = mmio_session
        .capture_ready_vsock_state(Some(vsock_config.clone()), &mmio_metrics, &mmio_idle_guard)
        .expect("inactive MMIO vsock should capture")
        .expect("configured MMIO vsock should be present");
    let HvfArm64BootVsockTransportState::Mmio { region, state, .. } = mmio_idle.transport() else {
        panic!("MMIO vsock should retain MMIO ownership");
    };
    let mmio_base = region.range().start();
    assert_eq!(mmio_base, GuestAddress::new(0x7000_0000));
    assert_eq!(
        mmio_idle.validation().reset_attempt(),
        VirtioVsockTransportResetAttempt::Inactive
    );
    assert!(!state.device().is_activated());
    assert_eq!(mmio_idle.config(), &vsock_config);
    drop(mmio_idle_guard);

    activate_vsock_capture_queues(&mut mmio_session, mmio_base, false);
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("active MMIO vsock auxiliary work should quiesce");
    let mmio = mmio_session
        .capture_ready_vsock_state(Some(vsock_config.clone()), &mmio_metrics, &mmio_guard)
        .expect("active MMIO vsock should reset and capture")
        .expect("active MMIO vsock should be present");
    assert!(matches!(
        mmio.validation().reset_attempt(),
        VirtioVsockTransportResetAttempt::Published(_)
    ));
    assert!(!mmio.validation().source_work().dropped_any_source_work());
    let active_mmio = mmio
        .transport()
        .mmio_state()
        .expect("active MMIO vsock should retain MMIO state");
    let queues = active_mmio
        .device()
        .active_queues()
        .expect("active MMIO capture should retain all queues");
    assert_eq!(queues.event().next_available(), 1);
    assert_eq!(queues.event().next_used(), 1);
    let mut reset_payload = [0_u8; 4];
    mmio_session
        .guest_memory()
        .expect("MMIO vsock guest memory should remain mapped")
        .read_slice(&mut reset_payload, VSOCK_CAPTURE_EVENT_PAYLOAD)
        .expect("MMIO reset payload should read");
    assert_eq!(
        u32::from_le_bytes(reset_payload),
        VIRTIO_VSOCK_EVENT_TRANSPORT_RESET
    );
    assert_eq!(mmio_metrics.snapshot().ev_queue_event_fails(), 0);
    drop(mmio_guard);

    notify_vsock_capture_event_queue(&mut mmio_session, mmio_base, false);
    let mmio_empty_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO empty-event reset capture should quiesce");
    let mmio_empty = mmio_session
        .capture_ready_vsock_state(Some(vsock_config.clone()), &mmio_metrics, &mmio_empty_guard)
        .expect("active MMIO vsock with an empty event queue should capture")
        .expect("configured MMIO vsock should remain present");
    assert_eq!(
        mmio_empty.validation().reset_attempt(),
        VirtioVsockTransportResetAttempt::QueueEmpty
    );
    assert!(
        !mmio_empty
            .validation()
            .source_work()
            .dropped_any_source_work()
    );
    drop(mmio_empty_guard);
    assert_eq!(mmio_metrics.snapshot().ev_queue_event_fails(), 1);

    for (descriptor_index, stage) in [
        HvfArm64BootVsockCaptureStage::InterruptDelivery,
        HvfArm64BootVsockCaptureStage::Capture,
        HvfArm64BootVsockCaptureStage::Handoff,
    ]
    .into_iter()
    .enumerate()
    {
        let descriptor_index = u16::try_from(descriptor_index + 1)
            .expect("vsock cancellation descriptor index should fit in u16");
        write_vsock_capture_event(
            mmio_session
                .guest_memory_mut()
                .expect("MMIO vsock guest memory should remain mapped"),
            descriptor_index,
            descriptor_index + 1,
        );
        let mmio_cancel_guard = mmio_session
            .quiesce_limiter_retry_wakeups()
            .expect("MMIO cancellation capture should quiesce");
        let cancelled = mmio_session
            .capture_ready_vsock_state_with_cancel(
                Some(vsock_config.clone()),
                &mmio_metrics,
                &mmio_cancel_guard,
                |candidate| candidate == stage,
            )
            .expect_err("post-reset cancellation should report after validation");
        assert_eq!(
            cancelled.disposition(),
            HvfArm64BootVsockCaptureDisposition::Recoverable
        );
        assert_eq!(cancelled.stage(), stage);
        drop(mmio_cancel_guard);
        notify_vsock_capture_event_queue(&mut mmio_session, mmio_base, false);
    }
    mmio_session
        .shutdown()
        .expect("signed MMIO vsock session should shut down");
    drop(mmio_session);
    assert!(!socket_path.exists());

    let mut pci_session =
        OwnedHvfArm64BootSession::new(&controller, base_session_config().with_pci_enabled())
            .expect("signed PCI vsock session should prepare");
    let pci_metrics = pci_session.shared_vsock_device_metrics();
    let pci_idle_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI vsock auxiliary work should quiesce");
    let pci_idle = pci_session
        .capture_ready_vsock_state(Some(vsock_config.clone()), &pci_metrics, &pci_idle_guard)
        .expect("inactive PCI vsock should capture")
        .expect("configured PCI vsock should be present");
    let HvfArm64BootVsockTransportState::Pci {
        sbdf,
        bar_range,
        state,
    } = pci_idle.transport()
    else {
        panic!("PCI vsock should retain PCI ownership");
    };
    let pci_base = bar_range.start();
    assert!(sbdf.device() > 0);
    assert_eq!(bar_range.size(), VIRTIO_PCI_CAPABILITY_BAR_SIZE);
    assert_eq!(
        pci_idle.validation().reset_attempt(),
        VirtioVsockTransportResetAttempt::Inactive
    );
    assert!(!state.device().is_activated());
    drop(pci_idle_guard);

    activate_vsock_capture_queues(&mut pci_session, pci_base, true);
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("active PCI vsock auxiliary work should quiesce");
    let pci = pci_session
        .capture_ready_vsock_state(Some(vsock_config.clone()), &pci_metrics, &pci_guard)
        .expect("active PCI vsock should reset and capture")
        .expect("active PCI vsock should be present");
    assert!(matches!(
        pci.validation().reset_attempt(),
        VirtioVsockTransportResetAttempt::Published(_)
    ));
    assert!(!pci.validation().source_work().dropped_any_source_work());
    let active_pci = pci
        .transport()
        .pci_state()
        .expect("active PCI vsock should retain PCI state");
    let queues = active_pci
        .device()
        .active_queues()
        .expect("active PCI capture should retain all queues");
    assert_eq!(queues.event().next_available(), 1);
    assert_eq!(queues.event().next_used(), 1);
    assert_eq!(pci_metrics.snapshot().ev_queue_event_fails(), 0);
    drop(pci_guard);
    notify_vsock_capture_event_queue(&mut pci_session, pci_base, true);
    let pci_empty_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI empty-event reset capture should quiesce");
    let pci_empty = pci_session
        .capture_ready_vsock_state(Some(vsock_config), &pci_metrics, &pci_empty_guard)
        .expect("active PCI vsock with an empty event queue should capture")
        .expect("configured PCI vsock should remain present");
    assert_eq!(
        pci_empty.validation().reset_attempt(),
        VirtioVsockTransportResetAttempt::QueueEmpty
    );
    assert!(
        !pci_empty
            .validation()
            .source_work()
            .dropped_any_source_work()
    );
    drop(pci_empty_guard);
    assert_eq!(pci_metrics.snapshot().ev_queue_event_fails(), 1);
    pci_session
        .shutdown()
        .expect("signed PCI vsock session should shut down");
    drop(pci_session);
    assert!(!socket_path.exists());
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_entropy_traverses_signed_mmio_and_pci_owners() {
    use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceTransport;
    use bangbang_runtime::snapshot_entropy_v2_8::SnapshotV2EntropyRetryState;

    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootEntropyCaptureError, HvfArm64BootEntropyDeviceConfig,
        HvfArm64BootEntropyTransportState, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::entropy::{
        EntropyConfigInput, EntropyMmioLayout, EntropyRateLimiterConfig, EntropyTokenBucketConfig,
        VirtioRngRetryCaptureState,
    };
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("capture-ready-entropy-kernel", &image)
        .expect("entropy capture kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("entropy capture boot source should configure");
    let rate_limiter = EntropyRateLimiterConfig::new(
        Some(EntropyTokenBucketConfig::new(4, None, 60_000)),
        Some(EntropyTokenBucketConfig::new(1, None, 60_000)),
    );
    controller
        .handle_action(VmmAction::PutEntropy(
            EntropyConfigInput::new().with_rate_limiter(rate_limiter),
        ))
        .expect("entropy capture device should configure");
    let entropy_config = controller
        .entropy_config()
        .expect("entropy capture config should exist");
    let entropy_device = HvfArm64BootEntropyDeviceConfig::new(EntropyMmioLayout::new(
        GuestAddress::new(0x4000_7000),
        MmioRegionId::new(3001),
    ));
    let base_session_config = || {
        HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            test_rtc_mmio_layout(),
        )
        .with_entropy_device(entropy_device)
    };

    let mut mmio_session = OwnedHvfArm64BootSession::new(&controller, base_session_config())
        .expect("signed MMIO entropy session should prepare");
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO entropy retry publisher should quiesce");
    let now = Instant::now();
    let mmio_first = mmio_session
        .capture_ready_entropy_state_at(Some(entropy_config), &mmio_guard, now)
        .expect("signed MMIO entropy should become capture-ready")
        .expect("configured MMIO entropy should be captured");
    let mmio_second = mmio_session
        .capture_ready_entropy_state_at(Some(entropy_config), &mmio_guard, now)
        .expect("signed MMIO entropy should support repeated detached capture")
        .expect("configured MMIO entropy should remain captured");
    assert_eq!(mmio_first, mmio_second);
    assert_eq!(mmio_first.config(), entropy_config);
    assert_eq!(mmio_first.retry(), VirtioRngRetryCaptureState::None);
    let HvfArm64BootEntropyTransportState::Mmio {
        region,
        interrupt_line,
        state,
    } = mmio_first.transport()
    else {
        panic!("MMIO entropy should retain MMIO ownership");
    };
    let mmio_base = region.range().start();
    assert_eq!(mmio_base, GuestAddress::new(0x4000_7000));
    assert!(interrupt_line.raw_value() > 0);
    assert_eq!(state.device().config(), entropy_config);
    assert!(state.device().active_queue().is_none());
    assert!(state.device().rate_limiter().bandwidth().is_some());
    assert!(state.device().rate_limiter().ops().is_some());
    assert!(!state.transport().is_device_activated());
    let mmio_snapshot = mmio_first
        .try_to_snapshot_v2()
        .expect("signed inactive MMIO capture should convert to exact 2.8");
    assert!(matches!(
        mmio_snapshot.transport(),
        SnapshotV2DeviceTransport::Mmio(_)
    ));
    assert_eq!(mmio_snapshot.active_queue(), None);
    assert_eq!(mmio_snapshot.retry(), SnapshotV2EntropyRetryState::None);
    assert!(!format!("{mmio_first:?}").contains("40007000"));
    assert!(matches!(
        mmio_session.capture_ready_entropy_state_at(None, &mmio_guard, now),
        Err(HvfArm64BootEntropyCaptureError::OwnershipMismatch {
            configured: false,
            mmio_owner: true,
            pci_owner: false,
        })
    ));
    drop(mmio_guard);

    let mmio_retry_after =
        activate_and_notify_entropy_capture_queue(&mut mmio_session, mmio_base, false);
    assert!(mmio_retry_after > std::time::Duration::ZERO);
    assert!(mmio_retry_after <= std::time::Duration::from_secs(60));
    let mmio_pending_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO pending entropy retry publisher should quiesce");
    let mmio_pending_now = Instant::now();
    let mmio_pending = mmio_session
        .capture_ready_entropy_state_at(Some(entropy_config), &mmio_pending_guard, mmio_pending_now)
        .expect("signed MMIO pending entropy should become capture-ready")
        .expect("configured MMIO pending entropy should be captured");
    let mmio_pending_again = mmio_session
        .capture_ready_entropy_state_at(Some(entropy_config), &mmio_pending_guard, mmio_pending_now)
        .expect("signed MMIO pending entropy capture should repeat")
        .expect("configured MMIO pending entropy should remain captured");
    assert_eq!(mmio_pending, mmio_pending_again);
    assert!(matches!(
        mmio_pending.retry(),
        VirtioRngRetryCaptureState::After { remaining_nanos }
            if remaining_nanos > 0 && remaining_nanos <= 60_000_000_000
    ));
    let mmio_pending_device = mmio_pending
        .transport()
        .mmio_state()
        .expect("pending MMIO capture should retain MMIO transport")
        .device();
    assert!(mmio_pending_device.has_pending_rate_limited_queue());
    assert_eq!(
        mmio_pending_device
            .active_queue()
            .map(|queue| (queue.next_available(), queue.next_used())),
        Some((1, 1))
    );
    let mmio_pending_snapshot = mmio_pending
        .try_to_snapshot_v2()
        .expect("signed pending MMIO capture should convert to exact 2.8");
    assert_eq!(
        mmio_pending_snapshot
            .active_queue()
            .map(|queue| (queue.next_available(), queue.next_used())),
        Some((2, 1))
    );
    assert!(mmio_pending_snapshot.has_pending_work());
    assert!(matches!(
        mmio_pending_snapshot.retry(),
        SnapshotV2EntropyRetryState::After { remaining_nanos }
            if remaining_nanos > 0 && remaining_nanos <= 60_000_000_000
    ));
    drop(mmio_pending_guard);
    mmio_session
        .shutdown()
        .expect("signed MMIO entropy session should shut down");

    let mut pci_session =
        OwnedHvfArm64BootSession::new(&controller, base_session_config().with_pci_enabled())
            .expect("signed PCI entropy session should prepare");
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI entropy retry publisher should quiesce");
    let pci = pci_session
        .capture_ready_entropy_state_at(Some(entropy_config), &pci_guard, Instant::now())
        .expect("signed PCI entropy should become capture-ready")
        .expect("configured PCI entropy should be captured");
    assert_eq!(pci.config(), entropy_config);
    assert_eq!(pci.retry(), VirtioRngRetryCaptureState::None);
    let HvfArm64BootEntropyTransportState::Pci {
        sbdf,
        bar_range,
        state,
    } = pci.transport()
    else {
        panic!("PCI entropy should retain PCI ownership");
    };
    let pci_bar_base = bar_range.start();
    assert!(sbdf.device() > 0);
    assert_eq!(bar_range.size(), VIRTIO_PCI_CAPABILITY_BAR_SIZE);
    assert_eq!(state.device().config(), entropy_config);
    assert!(state.device().active_queue().is_none());
    assert!(state.device().rate_limiter().bandwidth().is_some());
    assert!(state.device().rate_limiter().ops().is_some());
    assert!(!state.transport().is_device_activated());
    let pci_snapshot = pci
        .try_to_snapshot_v2()
        .expect("signed inactive PCI capture should convert to exact 2.8");
    assert!(matches!(
        pci_snapshot.transport(),
        SnapshotV2DeviceTransport::Pci(_)
    ));
    assert_eq!(pci_snapshot.active_queue(), None);
    assert_eq!(pci_snapshot.retry(), SnapshotV2EntropyRetryState::None);
    assert!(!format!("{pci:?}").contains("40007000"));
    drop(pci_guard);

    let pci_retry_after =
        activate_and_notify_entropy_capture_queue(&mut pci_session, pci_bar_base, true);
    assert!(pci_retry_after > std::time::Duration::ZERO);
    assert!(pci_retry_after <= std::time::Duration::from_secs(60));
    let pci_pending_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI pending entropy retry publisher should quiesce");
    let pci_pending_now = Instant::now();
    let pci_pending = pci_session
        .capture_ready_entropy_state_at(Some(entropy_config), &pci_pending_guard, pci_pending_now)
        .expect("signed PCI pending entropy should become capture-ready")
        .expect("configured PCI pending entropy should be captured");
    let pci_pending_again = pci_session
        .capture_ready_entropy_state_at(Some(entropy_config), &pci_pending_guard, pci_pending_now)
        .expect("signed PCI pending entropy capture should repeat")
        .expect("configured PCI pending entropy should remain captured");
    assert_eq!(pci_pending, pci_pending_again);
    assert!(matches!(
        pci_pending.retry(),
        VirtioRngRetryCaptureState::After { remaining_nanos }
            if remaining_nanos > 0 && remaining_nanos <= 60_000_000_000
    ));
    let pci_pending_device = pci_pending
        .transport()
        .pci_state()
        .expect("pending PCI capture should retain PCI transport")
        .device();
    assert!(pci_pending_device.has_pending_rate_limited_queue());
    assert_eq!(
        pci_pending_device
            .active_queue()
            .map(|queue| (queue.next_available(), queue.next_used())),
        Some((1, 1))
    );
    let pci_pending_snapshot = pci_pending
        .try_to_snapshot_v2()
        .expect("signed pending PCI capture should convert to exact 2.8");
    assert_eq!(
        pci_pending_snapshot
            .active_queue()
            .map(|queue| (queue.next_available(), queue.next_used())),
        Some((2, 1))
    );
    assert!(pci_pending_snapshot.has_pending_work());
    assert!(matches!(
        pci_pending_snapshot.retry(),
        SnapshotV2EntropyRetryState::After { remaining_nanos }
            if remaining_nanos > 0 && remaining_nanos <= 60_000_000_000
    ));
    drop(pci_pending_guard);
    pci_session
        .shutdown()
        .expect("signed PCI entropy session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_signed_serial_entropy_mmio_owners_with_exact_retry_semantics() {
    use std::io::Cursor;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootEntropyDeviceConfig, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSnapshotV2CaptureInput, HvfSnapshotV2BootState,
        HvfSnapshotV2EntropyMmioRestoreStage, HvfSnapshotV2EntropyState, HvfSnapshotV2NativePath,
        HvfSnapshotV2RestoredSerialShell, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::entropy::{
        EntropyConfigInput, EntropyMmioLayout, EntropyRateLimiterConfig, EntropyTokenBucketConfig,
        VirtioRngOsEntropySource,
    };
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::serial::{
        SerialMmioDevice, SharedSerialOutput, SharedSerialOutputBuffer,
    };
    use bangbang_runtime::snapshot_entropy_v2_8::{
        SnapshotV2EntropyRestorePlan, SnapshotV2EntropyRetryState, SnapshotV2EntropyState,
    };
    use bangbang_runtime::snapshot_serial_v2_7::SnapshotV2SerialState;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("restore-serial-entropy-kernel", &image)
        .expect("serial entropy kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("serial entropy boot source should configure");
    let limiter = EntropyRateLimiterConfig::new(
        Some(EntropyTokenBucketConfig::new(4, None, 60_000)),
        Some(EntropyTokenBucketConfig::new(1, None, 60_000)),
    );
    controller
        .handle_action(VmmAction::PutEntropy(
            EntropyConfigInput::new().with_rate_limiter(limiter),
        ))
        .expect("serial entropy device should configure");
    let entropy_config = controller
        .entropy_config()
        .expect("serial entropy configuration should exist");
    let entropy_layout =
        EntropyMmioLayout::new(GuestAddress::new(0x4000_7000), MmioRegionId::new(3001));
    let session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        bangbang_runtime::rtc::RtcMmioLayout::new(
            GuestAddress::new(0x4000_1000),
            MmioRegionId::new(10),
        ),
    )
    .with_entropy_device(HvfArm64BootEntropyDeviceConfig::new(entropy_layout))
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(20),
        GuestAddress::new(0x4000_2000),
        SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
    ));
    let mut source = OwnedHvfArm64BootSession::new(&controller, session_config)
        .expect("signed serial entropy source should prepare");
    let entropy_base = source
        .runtime_resources()
        .entropy_device
        .as_ref()
        .expect("source entropy owner should exist")
        .registration
        .address();

    let inactive_guard = source
        .quiesce_limiter_retry_wakeups()
        .expect("inactive entropy publishers should quiesce");
    let inactive_now = Instant::now();
    let inactive = source
        .capture_ready_entropy_state_at(Some(entropy_config), &inactive_guard, inactive_now)
        .expect("inactive entropy owner should capture")
        .expect("inactive entropy device should exist")
        .try_to_snapshot_v2()
        .expect("inactive entropy capture should convert");
    drop(inactive_guard);

    let retry_after = activate_and_notify_entropy_capture_queue(&mut source, entropy_base, false);
    assert!(retry_after > std::time::Duration::ZERO);
    let active_guard = source
        .quiesce_limiter_retry_wakeups()
        .expect("active entropy publishers should quiesce");
    let active_now = Instant::now();
    let delayed = source
        .capture_ready_entropy_state_at(Some(entropy_config), &active_guard, active_now)
        .expect("delayed entropy owner should capture")
        .expect("delayed entropy device should exist")
        .try_to_snapshot_v2()
        .expect("delayed entropy capture should convert");
    let serial_capture = source
        .capture_ready_serial_state(controller.serial_config().clone(), &active_guard)
        .expect("serial state should become capture-ready");
    let serial = SnapshotV2SerialState::try_from_capture_ready(serial_capture)
        .expect("serial capture should convert to exact 2.7");
    drop(active_guard);
    assert!(matches!(
        delayed.retry(),
        SnapshotV2EntropyRetryState::After { .. }
    ));
    let (config, queue, limiter, _retry, pending, virtio, transport) = delayed.clone().into_parts();
    let immediate = SnapshotV2EntropyState::try_new(
        config,
        queue,
        limiter,
        SnapshotV2EntropyRetryState::Immediate,
        pending,
        virtio,
        transport,
    )
    .expect("pending delayed state should admit immediate retry");

    source
        .pause_for_snapshot_v2_capture()
        .expect("serial entropy source should pause");
    let boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_new(kernel.path().as_os_str())
            .expect("serial entropy kernel path should validate"),
        None,
        None,
    )
    .expect("serial entropy boot metadata should validate");
    let mut memory_writer = Cursor::new(Vec::new());
    let platform = source
        .capture_snapshot_v2_entropy_platform_with_cancel(
            HvfArm64BootSnapshotV2CaptureInput::new(boot),
            &mut memory_writer,
            |_| false,
        )
        .expect("exact-2.8 serial entropy platform should capture");
    for entropy in [&inactive, &delayed, &immediate] {
        HvfSnapshotV2EntropyState::try_new(
            platform.clone(),
            None,
            serial.clone(),
            Some(entropy.clone()),
        )
        .expect("exact-2.8 serial entropy composition should validate");
    }

    let layout = GuestMemoryLayout::new(source.runtime_resources().layout.ranges().to_vec())
        .expect("serial entropy destination layout should validate");
    let source_memory = source
        .guest_memory()
        .expect("serial entropy source memory should remain mapped");
    let mut destination_memories = Vec::new();
    destination_memories
        .try_reserve_exact(4)
        .expect("destination memory vector should reserve");
    for _ in 0..4 {
        let mut destination =
            GuestMemory::allocate(&layout).expect("serial entropy destination should allocate");
        let mut buffer = vec![0_u8; 64 * 1024];
        for range in layout.ranges() {
            let mut copied = 0_u64;
            while copied < range.size() {
                let remaining = range.size() - copied;
                let count =
                    usize::try_from(remaining.min(
                        u64::try_from(buffer.len()).expect("copy buffer length should fit u64"),
                    ))
                    .expect("copy size should fit usize");
                let address = range
                    .start()
                    .checked_add(copied)
                    .expect("copy address should fit");
                source_memory
                    .read_slice(&mut buffer[..count], address)
                    .expect("source guest bytes should read");
                destination
                    .write_slice(&buffer[..count], address)
                    .expect("destination guest bytes should write");
                copied += u64::try_from(count).expect("copy count should fit u64");
            }
        }
        destination_memories.push(destination);
    }
    source
        .shutdown()
        .expect("signed serial entropy source should shut down");

    let restored_shell = || {
        HvfSnapshotV2RestoredSerialShell::new(
            SerialMmioDevice::from_capture_state_with_shared_output(
                SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
                serial.device().clone(),
            ),
        )
    };

    let fault_now = Instant::now();
    let fault_plan = SnapshotV2EntropyRestorePlan::prepare(
        inactive.clone(),
        &destination_memories[0],
        fault_now,
    )
    .expect("fault entropy plan should prepare");
    let fault =
        OwnedHvfArm64BootSession::restore_snapshot_v2_serial_entropy_mmio_with_scheduler_fault(
            platform.clone(),
            destination_memories.remove(0),
            restored_shell(),
            None,
            fault_plan,
        )
        .expect_err("injected entropy scheduler fault should reject");
    if fault.stage() != HvfSnapshotV2EntropyMmioRestoreStage::RetryScheduler {
        let primary = std::error::Error::source(&fault);
        let nested = primary.and_then(std::error::Error::source);
        let root = nested.and_then(std::error::Error::source);
        panic!(
            "injected scheduler fault stopped at {:?}: primary={primary:?} nested={nested:?} root={root:?}",
            fault.stage()
        );
    }
    assert!(fault.is_terminal());
    assert!(!fault.has_incomplete_cleanup());
    let diagnostics = format!("{fault:?} {fault}");
    assert!(diagnostics.contains("<redacted>"));
    assert!(!diagnostics.contains("1073770496"));

    let source_constructions = Arc::new(AtomicUsize::new(0));
    for (index, (name, expected)) in [
        ("none", inactive),
        ("delayed", delayed),
        ("immediate", immediate),
    ]
    .into_iter()
    .enumerate()
    {
        let restore_now = Instant::now();
        let plan = SnapshotV2EntropyRestorePlan::prepare(
            expected.clone(),
            &destination_memories[0],
            restore_now,
        )
        .unwrap_or_else(|error| panic!("{name} entropy plan should prepare: {error:?}"));
        let construction_counter = Arc::clone(&source_constructions);
        let owners =
            OwnedHvfArm64BootSession::restore_snapshot_v2_serial_entropy_mmio_with_source_factory(
                platform.clone(),
                destination_memories.remove(0),
                restored_shell(),
                None,
                plan,
                move || {
                    construction_counter.fetch_add(1, Ordering::SeqCst);
                    VirtioRngOsEntropySource::new()
                },
            )
            .unwrap_or_else(|error| panic!("{name} entropy owners should restore: {error:?}"));
        assert_eq!(
            source_constructions.load(Ordering::SeqCst),
            index + 1,
            "{name} destination should construct exactly one fresh entropy source"
        );
        assert_eq!(owners.entropy_config(), entropy_config);
        assert!(owners.storage_configs().is_none());
        assert!(
            owners
                .session()
                .shared_entropy_device_metrics()
                .snapshot()
                .is_empty()
        );
        let (mut destination, returned_config, storage_configs) = owners.into_parts();
        assert_eq!(returned_config, entropy_config);
        assert!(storage_configs.is_none());
        assert!(destination.runtime_resources().entropy_device.is_some());
        assert!(destination.runtime_resources().pci_entropy_device.is_none());
        let guard = destination
            .quiesce_limiter_retry_wakeups()
            .unwrap_or_else(|error| panic!("{name} retry publishers should quiesce: {error:?}"));
        let recaptured = destination
            .capture_ready_entropy_state_at(Some(returned_config), &guard, restore_now)
            .unwrap_or_else(|error| panic!("{name} entropy owner should recapture: {error:?}"))
            .expect("restored entropy device should exist")
            .try_to_snapshot_v2()
            .unwrap_or_else(|error| panic!("{name} entropy recapture should convert: {error:?}"));
        assert_eq!(recaptured, expected, "{name} entropy state should be exact");
        assert!(
            destination
                .shared_entropy_device_metrics()
                .snapshot()
                .is_empty()
        );
        drop(guard);
        if name == "none" {
            let transport_base = destination
                .runtime_resources()
                .entropy_device
                .as_ref()
                .expect("restored entropy MMIO metadata should exist")
                .registration
                .address();
            let retry_after =
                activate_and_notify_entropy_capture_queue(&mut destination, transport_base, false);
            assert!(retry_after > std::time::Duration::ZERO);
            assert!(
                !destination
                    .shared_entropy_device_metrics()
                    .snapshot()
                    .is_empty()
            );
        }
        destination
            .shutdown()
            .unwrap_or_else(|error| panic!("{name} entropy destination should shut down: {error}"));
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_signed_serial_entropy_pci_owners_with_exact_retry_semantics() {
    use std::io::Cursor;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootEntropyDeviceConfig, HvfArm64BootEntropyTransportState,
        HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSnapshotV2CaptureInput, HvfSnapshotV2BootState,
        HvfSnapshotV2EntropyPciRestoreStage, HvfSnapshotV2EntropyState, HvfSnapshotV2NativePath,
        HvfSnapshotV2RestoredSerialShell, OwnedHvfArm64BootSession,
        prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::entropy::{
        EntropyConfigInput, EntropyMmioLayout, EntropyRateLimiterConfig, EntropyTokenBucketConfig,
        VirtioRngOsEntropySource,
    };
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::serial::{
        SerialMmioDevice, SharedSerialOutput, SharedSerialOutputBuffer,
    };
    use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceTransport;
    use bangbang_runtime::snapshot_entropy_v2_8::{
        SnapshotV2EntropyRestorePlan, SnapshotV2EntropyRetryState, SnapshotV2EntropyState,
    };
    use bangbang_runtime::snapshot_serial_v2_7::SnapshotV2SerialState;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("restore-serial-entropy-pci-kernel", &image)
        .expect("serial PCI entropy kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("serial PCI entropy boot source should configure");
    let limiter = EntropyRateLimiterConfig::new(
        Some(EntropyTokenBucketConfig::new(4, None, 60_000)),
        Some(EntropyTokenBucketConfig::new(1, None, 60_000)),
    );
    controller
        .handle_action(VmmAction::PutEntropy(
            EntropyConfigInput::new().with_rate_limiter(limiter),
        ))
        .expect("serial PCI entropy device should configure");
    let entropy_config = controller
        .entropy_config()
        .expect("serial PCI entropy configuration should exist");
    let session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        bangbang_runtime::rtc::RtcMmioLayout::new(
            GuestAddress::new(0x4000_1000),
            MmioRegionId::new(10),
        ),
    )
    .with_entropy_device(HvfArm64BootEntropyDeviceConfig::new(
        EntropyMmioLayout::new(GuestAddress::new(0x4000_7000), MmioRegionId::new(3001)),
    ))
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(20),
        GuestAddress::new(0x4000_2000),
        SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
    ))
    .with_pci_enabled();
    let mut source = OwnedHvfArm64BootSession::new(&controller, session_config)
        .expect("signed serial PCI entropy source should prepare");

    let inactive_guard = source
        .quiesce_limiter_retry_wakeups()
        .expect("inactive PCI entropy publishers should quiesce");
    let inactive_now = Instant::now();
    let inactive_capture = source
        .capture_ready_entropy_state_at(Some(entropy_config), &inactive_guard, inactive_now)
        .expect("inactive PCI entropy owner should capture")
        .expect("inactive PCI entropy device should exist");
    let entropy_base = match inactive_capture.transport() {
        HvfArm64BootEntropyTransportState::Pci { bar_range, .. } => bar_range.start(),
        HvfArm64BootEntropyTransportState::Mmio { .. } => {
            panic!("PCI-enabled entropy source should own a PCI endpoint")
        }
    };
    let inactive = inactive_capture
        .try_to_snapshot_v2()
        .expect("inactive PCI entropy capture should convert");
    drop(inactive_guard);

    let retry_after = activate_and_notify_entropy_capture_queue(&mut source, entropy_base, true);
    assert!(retry_after > std::time::Duration::ZERO);
    let active_guard = source
        .quiesce_limiter_retry_wakeups()
        .expect("active PCI entropy publishers should quiesce");
    let active_now = Instant::now();
    let delayed = source
        .capture_ready_entropy_state_at(Some(entropy_config), &active_guard, active_now)
        .expect("delayed PCI entropy owner should capture")
        .expect("delayed PCI entropy device should exist")
        .try_to_snapshot_v2()
        .expect("delayed PCI entropy capture should convert");
    let serial_capture = source
        .capture_ready_serial_state(controller.serial_config().clone(), &active_guard)
        .expect("serial state should become capture-ready");
    let serial = SnapshotV2SerialState::try_from_capture_ready(serial_capture)
        .expect("serial capture should convert to exact 2.7");
    drop(active_guard);
    assert!(matches!(
        delayed.retry(),
        SnapshotV2EntropyRetryState::After { .. }
    ));
    let (config, queue, limiter, _retry, pending, virtio, transport) = delayed.clone().into_parts();
    let immediate = SnapshotV2EntropyState::try_new(
        config,
        queue,
        limiter,
        SnapshotV2EntropyRetryState::Immediate,
        pending,
        virtio,
        transport,
    )
    .expect("pending delayed PCI state should admit immediate retry");

    source
        .pause_for_snapshot_v2_capture()
        .expect("serial PCI entropy source should pause");
    let boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_new(kernel.path().as_os_str())
            .expect("serial PCI entropy kernel path should validate"),
        None,
        None,
    )
    .expect("serial PCI entropy boot metadata should validate");
    let mut memory_writer = Cursor::new(Vec::new());
    let platform = source
        .capture_snapshot_v2_entropy_platform_with_cancel(
            HvfArm64BootSnapshotV2CaptureInput::new(boot),
            &mut memory_writer,
            |_| false,
        )
        .expect("exact-2.8 serial PCI entropy platform should capture");
    for entropy in [&inactive, &delayed, &immediate] {
        HvfSnapshotV2EntropyState::try_new(
            platform.clone(),
            None,
            serial.clone(),
            Some(entropy.clone()),
        )
        .expect("exact-2.8 serial PCI entropy composition should validate");
        assert!(matches!(
            entropy.transport(),
            SnapshotV2DeviceTransport::Pci(_)
        ));
    }

    let layout = GuestMemoryLayout::new(source.runtime_resources().layout.ranges().to_vec())
        .expect("serial PCI entropy destination layout should validate");
    let source_memory = source
        .guest_memory()
        .expect("serial PCI entropy source memory should remain mapped");
    let mut destination_memories = Vec::new();
    destination_memories
        .try_reserve_exact(4)
        .expect("PCI destination memory vector should reserve");
    for _ in 0..4 {
        let mut destination =
            GuestMemory::allocate(&layout).expect("serial PCI entropy destination should allocate");
        let mut buffer = vec![0_u8; 64 * 1024];
        for range in layout.ranges() {
            let mut copied = 0_u64;
            while copied < range.size() {
                let remaining = range.size() - copied;
                let count =
                    usize::try_from(remaining.min(
                        u64::try_from(buffer.len()).expect("copy buffer length should fit u64"),
                    ))
                    .expect("copy size should fit usize");
                let address = range
                    .start()
                    .checked_add(copied)
                    .expect("copy address should fit");
                source_memory
                    .read_slice(&mut buffer[..count], address)
                    .expect("source guest bytes should read");
                destination
                    .write_slice(&buffer[..count], address)
                    .expect("destination guest bytes should write");
                copied += u64::try_from(count).expect("copy count should fit u64");
            }
        }
        destination_memories.push(destination);
    }
    source
        .shutdown()
        .expect("signed serial PCI entropy source should shut down");

    let restored_shell = || {
        HvfSnapshotV2RestoredSerialShell::new(
            SerialMmioDevice::from_capture_state_with_shared_output(
                SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
                serial.device().clone(),
            ),
        )
    };

    let fault_now = Instant::now();
    let fault_plan = SnapshotV2EntropyRestorePlan::prepare(
        inactive.clone(),
        &destination_memories[0],
        fault_now,
    )
    .expect("fault PCI entropy plan should prepare");
    let fault_endpoint_plan =
        prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan(&platform, &fault_plan)
            .expect("fault PCI entropy platform plan should prepare");
    let fault =
        OwnedHvfArm64BootSession::restore_snapshot_v2_serial_entropy_pci_with_scheduler_fault(
            platform.clone(),
            destination_memories.remove(0),
            restored_shell(),
            None,
            fault_endpoint_plan,
            fault_plan,
        )
        .expect_err("injected PCI entropy scheduler fault should reject");
    assert_eq!(
        fault.stage(),
        HvfSnapshotV2EntropyPciRestoreStage::RetryScheduler
    );
    assert!(fault.is_terminal());
    assert!(!fault.has_incomplete_cleanup());
    let diagnostics = format!("{fault:?} {fault}");
    assert!(diagnostics.contains("<redacted>"));
    assert!(!diagnostics.contains(&entropy_base.raw_value().to_string()));

    let source_constructions = Arc::new(AtomicUsize::new(0));
    for (index, (name, expected)) in [
        ("none", inactive),
        ("delayed", delayed),
        ("immediate", immediate),
    ]
    .into_iter()
    .enumerate()
    {
        let restore_now = Instant::now();
        let restore_plan = SnapshotV2EntropyRestorePlan::prepare(
            expected.clone(),
            &destination_memories[0],
            restore_now,
        )
        .unwrap_or_else(|error| panic!("{name} PCI entropy plan should prepare: {error:?}"));
        let endpoint_plan =
            prepare_hvf_snapshot_v2_serial_entropy_pci_platform_plan(&platform, &restore_plan)
                .unwrap_or_else(|error| {
                    panic!("{name} PCI entropy platform plan should prepare: {error:?}")
                });
        let construction_counter = Arc::clone(&source_constructions);
        let owners =
            OwnedHvfArm64BootSession::restore_snapshot_v2_serial_entropy_pci_with_source_factory(
                platform.clone(),
                destination_memories.remove(0),
                restored_shell(),
                None,
                endpoint_plan,
                restore_plan,
                move || {
                    construction_counter.fetch_add(1, Ordering::SeqCst);
                    VirtioRngOsEntropySource::new()
                },
            )
            .unwrap_or_else(|error| panic!("{name} PCI entropy owners should restore: {error:?}"));
        assert_eq!(
            source_constructions.load(Ordering::SeqCst),
            index + 1,
            "{name} PCI destination should construct exactly one fresh entropy source"
        );
        assert_eq!(owners.entropy_config(), entropy_config);
        assert!(owners.storage_configs().is_none());
        assert!(
            owners
                .session()
                .shared_entropy_device_metrics()
                .snapshot()
                .is_empty()
        );
        let (mut destination, returned_config, storage_configs) = owners.into_parts();
        assert_eq!(returned_config, entropy_config);
        assert!(storage_configs.is_none());
        assert!(destination.runtime_resources().entropy_device.is_none());
        assert!(destination.runtime_resources().pci_entropy_device.is_none());
        assert!(destination.uses_pci_data_devices());
        let guard = destination
            .quiesce_limiter_retry_wakeups()
            .unwrap_or_else(|error| panic!("{name} retry publishers should quiesce: {error:?}"));
        let recaptured = destination
            .capture_ready_entropy_state_at(Some(returned_config), &guard, restore_now)
            .unwrap_or_else(|error| panic!("{name} PCI entropy owner should recapture: {error:?}"))
            .expect("restored PCI entropy device should exist")
            .try_to_snapshot_v2()
            .unwrap_or_else(|error| {
                panic!("{name} PCI entropy recapture should convert: {error:?}")
            });
        assert_eq!(
            recaptured, expected,
            "{name} PCI entropy state should be exact"
        );
        assert!(matches!(
            recaptured.transport(),
            SnapshotV2DeviceTransport::Pci(_)
        ));
        drop(guard);
        if name == "none" {
            let SnapshotV2DeviceTransport::Pci(transport) = recaptured.transport() else {
                panic!("restored inactive entropy should retain PCI placement");
            };
            let retry_after = activate_and_notify_entropy_capture_queue(
                &mut destination,
                transport.bar_range().start(),
                true,
            );
            assert!(retry_after > std::time::Duration::ZERO);
            assert!(
                !destination
                    .shared_entropy_device_metrics()
                    .snapshot()
                    .is_empty()
            );
        }
        destination.shutdown().unwrap_or_else(|error| {
            panic!("{name} PCI entropy destination should shut down: {error}")
        });
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_signed_storage_serial_entropy_mmio_owner_graph() {
    use std::io::Cursor;
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootEntropyDeviceConfig, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSnapshotV2CaptureInput, HvfSnapshotV2BootState, HvfSnapshotV2EntropyState,
        HvfSnapshotV2NativePath, HvfSnapshotV2RestoredSerialShell,
        HvfSnapshotV2StorageMmioProcessConfig, OwnedHvfArm64BootSession,
        prepare_hvf_snapshot_v2_storage_entropy_mmio_platform_plan,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{
        BlockFileBacking, BlockMmioLayout, DriveConfigInput, DriveIoEngine,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::entropy::{EntropyConfigInput, EntropyMmioLayout};
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::serial::{
        SerialMmioDevice, SharedSerialOutput, SharedSerialOutputBuffer,
    };
    use bangbang_runtime::snapshot_device_v2::SnapshotV2DeviceTransport;
    use bangbang_runtime::snapshot_device_v2_6::SnapshotV2StorageRestorePlan;
    use bangbang_runtime::snapshot_entropy_v2_8::SnapshotV2EntropyRestorePlan;
    use bangbang_runtime::snapshot_serial_v2_7::SnapshotV2SerialState;
    use bangbang_runtime::storage_capture::CaptureReadyStorageConfigs;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("restore-storage-entropy-kernel", &image)
        .expect("storage entropy kernel should create");
    let root = TempFile::new_len("restore-storage-entropy-root", 4096)
        .expect("storage entropy root should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("storage entropy boot source should configure");
    controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", root.path(), true)
                .with_is_read_only(true)
                .with_io_engine(DriveIoEngine::Sync),
        ))
        .expect("storage entropy root should configure");
    controller
        .handle_action(VmmAction::PutEntropy(EntropyConfigInput::new()))
        .expect("storage entropy device should configure");
    let entropy_config = controller
        .entropy_config()
        .expect("storage entropy configuration should exist");
    let block_layout = BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1));
    let pmem_layout = PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500));
    let session_config = HvfArm64BootSessionConfig::new(
        block_layout,
        pmem_layout,
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        bangbang_runtime::rtc::RtcMmioLayout::new(
            GuestAddress::new(0x4000_1000),
            MmioRegionId::new(10),
        ),
    )
    .with_entropy_device(HvfArm64BootEntropyDeviceConfig::new(
        EntropyMmioLayout::new(GuestAddress::new(0x4000_7000), MmioRegionId::new(3001)),
    ))
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(20),
        GuestAddress::new(0x4000_2000),
        SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
    ));
    let mut source = OwnedHvfArm64BootSession::new(&controller, session_config)
        .expect("signed storage entropy source should prepare");
    let source_configs =
        CaptureReadyStorageConfigs::new(controller.drive_configs().to_vec(), Vec::new());
    let guard = source
        .quiesce_limiter_retry_wakeups()
        .expect("storage entropy publishers should quiesce");
    let capture_now = Instant::now();
    let graph = source
        .capture_snapshot_v2_storage_device_graph_at(&source_configs, &guard, capture_now)
        .expect("storage graph should capture");
    let entropy = source
        .capture_ready_entropy_state_at(Some(entropy_config), &guard, capture_now)
        .expect("storage entropy owner should capture")
        .expect("storage entropy device should exist")
        .try_to_snapshot_v2()
        .expect("storage entropy capture should convert");
    let serial = SnapshotV2SerialState::try_from_capture_ready(
        source
            .capture_ready_serial_state(controller.serial_config().clone(), &guard)
            .expect("storage serial state should capture"),
    )
    .expect("storage serial capture should convert");
    drop(guard);

    source
        .pause_for_snapshot_v2_capture()
        .expect("storage entropy source should pause");
    let boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_new(kernel.path().as_os_str())
            .expect("storage entropy kernel path should validate"),
        None,
        None,
    )
    .expect("storage entropy boot metadata should validate");
    let mut memory_writer = Cursor::new(Vec::new());
    let platform = source
        .capture_snapshot_v2_entropy_platform_with_cancel(
            HvfArm64BootSnapshotV2CaptureInput::new(boot),
            &mut memory_writer,
            |_| false,
        )
        .expect("storage entropy exact-2.8 platform should capture");
    HvfSnapshotV2EntropyState::try_new(
        platform.clone(),
        Some(graph.clone()),
        serial.clone(),
        Some(entropy.clone()),
    )
    .expect("storage, serial, and entropy composition should validate");

    let layout = GuestMemoryLayout::new(source.runtime_resources().layout.ranges().to_vec())
        .expect("storage entropy destination layout should validate");
    let mut destination =
        GuestMemory::allocate(&layout).expect("storage entropy destination should allocate");
    let source_memory = source
        .guest_memory()
        .expect("storage entropy source memory should remain mapped");
    let mut buffer = vec![0_u8; 64 * 1024];
    for range in layout.ranges() {
        let mut copied = 0_u64;
        while copied < range.size() {
            let remaining = range.size() - copied;
            let count = usize::try_from(
                remaining
                    .min(u64::try_from(buffer.len()).expect("copy buffer length should fit u64")),
            )
            .expect("copy size should fit usize");
            let address = range
                .start()
                .checked_add(copied)
                .expect("copy address should fit");
            source_memory
                .read_slice(&mut buffer[..count], address)
                .expect("storage entropy source bytes should read");
            destination
                .write_slice(&buffer[..count], address)
                .expect("storage entropy destination bytes should write");
            copied += u64::try_from(count).expect("copy count should fit u64");
        }
    }
    source
        .shutdown()
        .expect("signed storage entropy source should shut down");

    let restore_now = Instant::now();
    let entropy_plan =
        SnapshotV2EntropyRestorePlan::prepare(entropy.clone(), &destination, restore_now)
            .expect("storage entropy restore plan should prepare");
    let backing = BlockFileBacking::open_snapshot(
        std::path::Path::new(graph.block_records()[0].config().selector()),
        graph.block_records()[0].config().is_read_only(),
    )
    .expect("storage entropy block backing should reopen")
    .0;
    let bundle = SnapshotV2StorageRestorePlan::prepare(graph.clone(), &destination, restore_now)
        .expect("storage entropy graph restore plan should prepare")
        .prepare_backings(vec![backing], Vec::new(), || false)
        .expect("storage entropy backing bundle should prepare");
    let entropy_interrupt = match entropy.transport() {
        SnapshotV2DeviceTransport::Mmio(mmio) => mmio.interrupt_line(),
        SnapshotV2DeviceTransport::Pci(_) => panic!("storage entropy fixture should use MMIO"),
    };
    let storage_plan = prepare_hvf_snapshot_v2_storage_entropy_mmio_platform_plan(
        &platform,
        &bundle,
        HvfSnapshotV2StorageMmioProcessConfig::new(block_layout, pmem_layout),
        entropy_interrupt,
    )
    .expect("storage entropy platform plan should prepare");
    let shell = HvfSnapshotV2RestoredSerialShell::new(
        SerialMmioDevice::from_capture_state_with_shared_output(
            SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
            serial.device().clone(),
        ),
    );
    let owners = OwnedHvfArm64BootSession::restore_snapshot_v2_serial_storage_entropy_mmio(
        platform,
        destination,
        shell,
        None,
        bundle,
        storage_plan,
        entropy_plan,
    )
    .unwrap_or_else(|error| panic!("storage entropy owners should restore: {error:?}"));
    assert_eq!(owners.entropy_config(), entropy_config);
    assert_eq!(
        owners
            .storage_configs()
            .expect("restored storage configurations should exist"),
        &source_configs
    );
    let (mut restored, returned_entropy_config, returned_storage_configs) = owners.into_parts();
    let returned_storage_configs =
        returned_storage_configs.expect("restored storage configurations should be retained");
    assert_eq!(returned_storage_configs, source_configs);
    assert_eq!(restored.runtime_resources().block_devices.len(), 1);
    assert!(restored.runtime_resources().entropy_device.is_some());
    assert!(restored.runtime_resources().pci_entropy_device.is_none());
    assert!(
        restored
            .shared_entropy_device_metrics()
            .snapshot()
            .is_empty()
    );

    let guard = restored
        .quiesce_limiter_retry_wakeups()
        .expect("restored storage entropy publishers should quiesce");
    let recaptured_entropy = restored
        .capture_ready_entropy_state_at(Some(returned_entropy_config), &guard, restore_now)
        .expect("restored storage entropy owner should capture")
        .expect("restored storage entropy device should exist")
        .try_to_snapshot_v2()
        .expect("restored storage entropy capture should convert");
    assert_eq!(recaptured_entropy, entropy);
    let recaptured_graph = restored
        .capture_snapshot_v2_storage_device_graph_at(&returned_storage_configs, &guard, restore_now)
        .expect("restored storage graph should recapture");
    assert_eq!(recaptured_graph, graph);
    drop(guard);
    restored
        .shutdown()
        .expect("restored storage entropy destination should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn restores_signed_storage_serial_entropy_pci_owner_graph() {
    use std::io::Cursor;
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootEntropyDeviceConfig, HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig,
        HvfArm64BootSnapshotV2CaptureInput, HvfSnapshotV2BootState, HvfSnapshotV2EntropyState,
        HvfSnapshotV2NativePath, HvfSnapshotV2RestoredSerialShell, OwnedHvfArm64BootSession,
        prepare_hvf_snapshot_v2_storage_entropy_pci_platform_plan,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{
        BlockFileBacking, BlockMmioLayout, DriveConfigInput, DriveIoEngine,
    };
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::entropy::{EntropyConfigInput, EntropyMmioLayout};
    use bangbang_runtime::memory::{GuestAddress, GuestMemory, GuestMemoryLayout};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::serial::{
        SerialMmioDevice, SharedSerialOutput, SharedSerialOutputBuffer,
    };
    use bangbang_runtime::snapshot_device_v2::{
        SnapshotV2DeviceTransport, SnapshotV2DeviceTransportKind,
    };
    use bangbang_runtime::snapshot_device_v2_6::SnapshotV2StorageRestorePlan;
    use bangbang_runtime::snapshot_entropy_v2_8::SnapshotV2EntropyRestorePlan;
    use bangbang_runtime::snapshot_serial_v2_7::SnapshotV2SerialState;
    use bangbang_runtime::storage_capture::CaptureReadyStorageConfigs;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("restore-storage-entropy-pci-kernel", &image)
        .expect("storage PCI entropy kernel should create");
    let root = TempFile::new_len("restore-storage-entropy-pci-root", 4096)
        .expect("storage PCI entropy root should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("storage PCI entropy boot source should configure");
    controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", root.path(), true)
                .with_is_read_only(true)
                .with_io_engine(DriveIoEngine::Sync),
        ))
        .expect("storage PCI entropy root should configure");
    controller
        .handle_action(VmmAction::PutEntropy(EntropyConfigInput::new()))
        .expect("storage PCI entropy device should configure");
    let entropy_config = controller
        .entropy_config()
        .expect("storage PCI entropy configuration should exist");
    let session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        bangbang_runtime::rtc::RtcMmioLayout::new(
            GuestAddress::new(0x4000_1000),
            MmioRegionId::new(10),
        ),
    )
    .with_entropy_device(HvfArm64BootEntropyDeviceConfig::new(
        EntropyMmioLayout::new(GuestAddress::new(0x4000_7000), MmioRegionId::new(3001)),
    ))
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(20),
        GuestAddress::new(0x4000_2000),
        SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
    ))
    .with_pci_enabled();
    let mut source = OwnedHvfArm64BootSession::new(&controller, session_config)
        .expect("signed storage PCI entropy source should prepare");
    let source_configs =
        CaptureReadyStorageConfigs::new(controller.drive_configs().to_vec(), Vec::new());
    let guard = source
        .quiesce_limiter_retry_wakeups()
        .expect("storage PCI entropy publishers should quiesce");
    let capture_now = Instant::now();
    let graph = source
        .capture_snapshot_v2_storage_device_graph_at(&source_configs, &guard, capture_now)
        .expect("PCI storage graph should capture");
    assert_eq!(graph.transport_kind(), SnapshotV2DeviceTransportKind::Pci);
    let entropy = source
        .capture_ready_entropy_state_at(Some(entropy_config), &guard, capture_now)
        .expect("storage PCI entropy owner should capture")
        .expect("storage PCI entropy device should exist")
        .try_to_snapshot_v2()
        .expect("storage PCI entropy capture should convert");
    assert!(matches!(
        entropy.transport(),
        SnapshotV2DeviceTransport::Pci(_)
    ));
    let serial = SnapshotV2SerialState::try_from_capture_ready(
        source
            .capture_ready_serial_state(controller.serial_config().clone(), &guard)
            .expect("storage PCI serial state should capture"),
    )
    .expect("storage PCI serial capture should convert");
    drop(guard);

    source
        .pause_for_snapshot_v2_capture()
        .expect("storage PCI entropy source should pause");
    let boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_new(kernel.path().as_os_str())
            .expect("storage PCI entropy kernel path should validate"),
        None,
        None,
    )
    .expect("storage PCI entropy boot metadata should validate");
    let mut memory_writer = Cursor::new(Vec::new());
    let platform = source
        .capture_snapshot_v2_entropy_platform_with_cancel(
            HvfArm64BootSnapshotV2CaptureInput::new(boot),
            &mut memory_writer,
            |_| false,
        )
        .expect("storage PCI entropy exact-2.8 platform should capture");
    HvfSnapshotV2EntropyState::try_new(
        platform.clone(),
        Some(graph.clone()),
        serial.clone(),
        Some(entropy.clone()),
    )
    .expect("storage, serial, and PCI entropy composition should validate");

    let layout = GuestMemoryLayout::new(source.runtime_resources().layout.ranges().to_vec())
        .expect("storage PCI entropy destination layout should validate");
    let mut destination =
        GuestMemory::allocate(&layout).expect("storage PCI entropy destination should allocate");
    let source_memory = source
        .guest_memory()
        .expect("storage PCI entropy source memory should remain mapped");
    let mut buffer = vec![0_u8; 64 * 1024];
    for range in layout.ranges() {
        let mut copied = 0_u64;
        while copied < range.size() {
            let remaining = range.size() - copied;
            let count = usize::try_from(
                remaining
                    .min(u64::try_from(buffer.len()).expect("copy buffer length should fit u64")),
            )
            .expect("copy size should fit usize");
            let address = range
                .start()
                .checked_add(copied)
                .expect("copy address should fit");
            source_memory
                .read_slice(&mut buffer[..count], address)
                .expect("storage PCI entropy source bytes should read");
            destination
                .write_slice(&buffer[..count], address)
                .expect("storage PCI entropy destination bytes should write");
            copied += u64::try_from(count).expect("copy count should fit u64");
        }
    }
    source
        .shutdown()
        .expect("signed storage PCI entropy source should shut down");

    let restore_now = Instant::now();
    let entropy_plan =
        SnapshotV2EntropyRestorePlan::prepare(entropy.clone(), &destination, restore_now)
            .expect("storage PCI entropy restore plan should prepare");
    let backing = BlockFileBacking::open_snapshot(
        std::path::Path::new(graph.block_records()[0].config().selector()),
        graph.block_records()[0].config().is_read_only(),
    )
    .expect("storage PCI entropy block backing should reopen")
    .0;
    let bundle = SnapshotV2StorageRestorePlan::prepare(graph.clone(), &destination, restore_now)
        .expect("storage PCI entropy graph restore plan should prepare")
        .prepare_backings(vec![backing], Vec::new(), || false)
        .expect("storage PCI entropy backing bundle should prepare");
    let combined_plan = prepare_hvf_snapshot_v2_storage_entropy_pci_platform_plan(
        &platform,
        &bundle,
        &entropy_plan,
    )
    .expect("storage PCI entropy combined plan should prepare");
    assert_eq!(combined_plan.storage().pci().record_count(), 1);
    assert_eq!(combined_plan.entropy().preceding_endpoint_count(), 1);
    let shell = HvfSnapshotV2RestoredSerialShell::new(
        SerialMmioDevice::from_capture_state_with_shared_output(
            SharedSerialOutput::from(SharedSerialOutputBuffer::default()),
            serial.device().clone(),
        ),
    );
    let owners = OwnedHvfArm64BootSession::restore_snapshot_v2_serial_storage_entropy_pci(
        platform,
        destination,
        shell,
        None,
        bundle,
        combined_plan,
        entropy_plan,
    )
    .unwrap_or_else(|error| panic!("storage PCI entropy owners should restore: {error:?}"));
    assert_eq!(owners.entropy_config(), entropy_config);
    assert_eq!(
        owners
            .storage_configs()
            .expect("restored PCI storage configurations should exist"),
        &source_configs
    );
    let (mut restored, returned_entropy_config, returned_storage_configs) = owners.into_parts();
    let returned_storage_configs =
        returned_storage_configs.expect("restored PCI storage configurations should be retained");
    assert_eq!(returned_storage_configs, source_configs);
    assert!(restored.uses_pci_data_devices());
    assert!(restored.runtime_resources().block_devices.is_empty());
    assert!(restored.runtime_resources().entropy_device.is_none());
    assert!(restored.runtime_resources().pci_entropy_device.is_none());
    assert!(
        restored
            .shared_entropy_device_metrics()
            .snapshot()
            .is_empty()
    );

    let guard = restored
        .quiesce_limiter_retry_wakeups()
        .expect("restored storage PCI entropy publishers should quiesce");
    let recaptured_entropy = restored
        .capture_ready_entropy_state_at(Some(returned_entropy_config), &guard, restore_now)
        .expect("restored storage PCI entropy owner should capture")
        .expect("restored storage PCI entropy device should exist")
        .try_to_snapshot_v2()
        .expect("restored storage PCI entropy capture should convert");
    assert_eq!(recaptured_entropy, entropy);
    let recaptured_graph = restored
        .capture_snapshot_v2_storage_device_graph_at(&returned_storage_configs, &guard, restore_now)
        .expect("restored PCI storage graph should recapture");
    assert_eq!(recaptured_graph, graph);
    drop(guard);
    restored
        .shutdown()
        .expect("restored storage PCI entropy destination should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn capture_ready_memory_hotplug_traverses_signed_mmio_and_pci_owners() {
    use bangbang_hvf::{
        HvfArm64BootMemoryHotplugCaptureError, HvfArm64BootMemoryHotplugDeviceConfig,
        HvfArm64BootMemoryHotplugTransportState, HvfArm64BootSessionConfig,
        OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::memory_hotplug::{MemoryHotplugConfigInput, VirtioMemMmioLayout};
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::virtio_pci::VIRTIO_PCI_CAPABILITY_BAR_SIZE;
    use bangbang_runtime::vsock::VsockMmioLayout;

    const MIB: u64 = 1024 * 1024;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("capture-ready-memory-hotplug-kernel", &image)
        .expect("memory-hotplug capture kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("memory-hotplug capture boot source should configure");
    controller
        .handle_action(VmmAction::PutMemoryHotplug(MemoryHotplugConfigInput::new(
            128, 2, 128,
        )))
        .expect("memory-hotplug capture device should configure");
    let memory_hotplug_config = controller
        .memory_hotplug_config()
        .expect("memory-hotplug config should exist");
    let memory_hotplug_device = HvfArm64BootMemoryHotplugDeviceConfig::new(
        VirtioMemMmioLayout::new(GuestAddress::new(0x4000_8000), MmioRegionId::new(4001)),
    );
    let base_session_config = || {
        HvfArm64BootSessionConfig::new(
            BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
            PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
            NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
            VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
            test_rtc_mmio_layout(),
        )
        .with_memory_hotplug_device(memory_hotplug_device)
    };

    let mut mmio_session = OwnedHvfArm64BootSession::new(&controller, base_session_config())
        .expect("signed MMIO memory-hotplug session should prepare");
    let mmio_metrics = mmio_session
        .shared_memory_hotplug_device_metrics()
        .expect("MMIO memory-hotplug metrics should be retained");
    let mmio_guard = mmio_session
        .quiesce_limiter_retry_wakeups()
        .expect("MMIO auxiliary publishers should quiesce");
    let mmio_first = mmio_session
        .capture_ready_memory_hotplug_state(Some(memory_hotplug_config), &mmio_guard)
        .expect("signed MMIO memory-hotplug should become capture-ready")
        .expect("configured MMIO memory-hotplug should be captured");
    let mmio_second = mmio_session
        .capture_ready_memory_hotplug_state(Some(memory_hotplug_config), &mmio_guard)
        .expect("signed MMIO memory-hotplug should support repeated detached capture")
        .expect("configured MMIO memory-hotplug should remain captured");
    assert_eq!(mmio_first.config(), memory_hotplug_config);
    let HvfArm64BootMemoryHotplugTransportState::Mmio { state, .. } = mmio_first.transport() else {
        panic!("MMIO memory-hotplug should retain MMIO ownership");
    };
    assert!(state.device().active_queue().is_none());
    assert!(!state.transport().is_device_activated());
    assert_eq!(mmio_first.mapping().active_ranges(), []);
    assert_eq!(mmio_first.mapping().active_bytes(), 0);
    assert_eq!(mmio_first.mapping().offline_bytes(), 128 * MIB);
    assert_eq!(mmio_first.mapping().reservation().range().size(), 128 * MIB);
    assert_eq!(
        mmio_first.mapping().mapping_identity(),
        mmio_first.mapping().reservation().mapping_identity()
    );
    assert_eq!(mmio_first, mmio_second);
    assert!(!format!("{mmio_first:?}").contains("40008000"));
    assert!(matches!(
        mmio_session.capture_ready_memory_hotplug_state(None, &mmio_guard),
        Err(HvfArm64BootMemoryHotplugCaptureError::OwnershipMismatch {
            configured: false,
            mmio_owner: true,
            pci_owner: false,
        })
    ));
    drop(mmio_guard);
    mmio_session
        .shutdown()
        .expect("signed MMIO memory-hotplug session should shut down");
    assert_eq!(mmio_metrics.snapshot().teardown_count(), 1);
    assert_eq!(mmio_metrics.snapshot().teardown_fails(), 0);

    let mut pci_session =
        OwnedHvfArm64BootSession::new(&controller, base_session_config().with_pci_enabled())
            .expect("signed PCI memory-hotplug session should prepare");
    let pci_metrics = pci_session
        .shared_memory_hotplug_device_metrics()
        .expect("PCI memory-hotplug metrics should be retained");
    let pci_guard = pci_session
        .quiesce_limiter_retry_wakeups()
        .expect("PCI auxiliary publishers should quiesce");
    let pci = pci_session
        .capture_ready_memory_hotplug_state(Some(memory_hotplug_config), &pci_guard)
        .expect("signed PCI memory-hotplug should become capture-ready")
        .expect("configured PCI memory-hotplug should be captured");
    let HvfArm64BootMemoryHotplugTransportState::Pci {
        sbdf,
        bar_range,
        state,
    } = pci.transport()
    else {
        panic!("PCI memory-hotplug should retain PCI ownership");
    };
    assert!(sbdf.device() > 0);
    assert_eq!(bar_range.size(), VIRTIO_PCI_CAPABILITY_BAR_SIZE);
    assert!(state.device().active_queue().is_none());
    assert!(!state.transport().is_device_activated());
    assert_eq!(pci.mapping().active_ranges(), []);
    assert_eq!(pci.mapping().active_bytes(), 0);
    assert_eq!(pci.mapping().offline_bytes(), 128 * MIB);
    assert_eq!(pci.mapping().reservation().range().size(), 128 * MIB);
    assert_eq!(
        pci.mapping().mapping_identity(),
        pci.mapping().reservation().mapping_identity()
    );
    assert!(!format!("{pci:?}").contains("40008000"));
    drop(pci_guard);
    pci_session
        .shutdown()
        .expect("signed PCI memory-hotplug session should shut down");
    assert_eq!(pci_metrics.snapshot().teardown_count(), 1);
    assert_eq!(pci_metrics.snapshot().teardown_fails(), 0);
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn guest_write_to_writable_pmem_is_visible_before_any_pmem_flush() {
    use std::os::unix::fs::FileExt;

    use bangbang_hvf::{HvfArm64BootSessionConfig, HvfBackend, HvfVcpuRunStepOutcome};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{PmemConfigInput, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT};
    use bangbang_runtime::vsock::VsockMmioLayout;

    const WRITE_OFFSET: u64 = 4096;
    const WRITE_VALUE: u32 = 0x5a6b_7c8d;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("direct-pmem-kernel", &image)
        .expect("temp direct-pmem kernel should be created");
    let pmem = TempFile::new_len("direct-pmem-backing", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp direct-pmem backing should be created");
    let observer =
        std::fs::File::open(pmem.path()).expect("independent direct-pmem observer should open");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("direct-pmem boot source should configure");
    controller
        .handle_action(VmmAction::PutPmem(PmemConfigInput::new(
            "pmem0",
            path_text(pmem.path()),
        )))
        .expect("direct-pmem device should configure");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut backend = HvfBackend::new();
    let mut session = backend
        .prepare_arm64_boot_session(&controller, config)
        .expect("direct-pmem session should prepare");
    let boot_registers = session
        .boot_registers()
        .expect("direct-pmem session should retain boot registers");
    let target = session.runtime_resources().pmem_devices[0]
        .guest_range()
        .start()
        .checked_add(WRITE_OFFSET)
        .expect("direct-pmem guest target should fit");
    let program = arm64_store_u32_and_hvc_program(target.raw_value(), WRITE_VALUE);
    session
        .guest_memory_mut()
        .expect("direct-pmem session should expose ordinary guest memory")
        .write_slice(&program, boot_registers.kernel_entry)
        .expect("direct-pmem guest program should replace the test kernel entry");

    let mut before = [0_u8; std::mem::size_of::<u32>()];
    observer
        .read_exact_at(&mut before, WRITE_OFFSET)
        .expect("direct-pmem observer should read before the guest write");
    assert_eq!(before, [0; std::mem::size_of::<u32>()]);

    assert!(matches!(
        session
            .run_once_and_handle_mmio()
            .expect("direct-pmem guest should reach HVC without a mapping exit"),
        HvfVcpuRunStepOutcome::Hvc { exit, .. } if exit.immediate() == 0
    ));

    let mut observed = [0_u8; std::mem::size_of::<u32>()];
    observer
        .read_exact_at(&mut observed, WRITE_OFFSET)
        .expect("independent observer should read the live file mapping");
    assert_eq!(
        u32::from_le_bytes(observed),
        WRITE_VALUE,
        "guest writes must be visible through the backing descriptor before a virtio-pmem or teardown flush"
    );

    session
        .shutdown()
        .expect("direct-pmem session should shut down after the pre-flush observation");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn direct_pmem_mapping_has_bounded_process_memory_growth() {
    use bangbang_hvf::{HvfArm64BootSessionConfig, HvfBackend};
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{PmemConfig, PmemConfigInput, PmemFileBacking, PmemMmioLayout};
    use bangbang_runtime::vsock::VsockMmioLayout;

    const PMEM_LEN: u64 = 64 * 1024 * 1024;
    const VIRTUAL_SIZE_SLACK: u64 = 16 * 1024 * 1024;
    const RESIDENT_SIZE_SLACK: u64 = 32 * 1024 * 1024;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("bounded-pmem-kernel", &image)
        .expect("bounded-pmem kernel should be created");
    let pmem = TempFile::new_len("bounded-direct-pmem", PMEM_LEN)
        .expect("bounded direct-pmem backing should be created");
    let pmem_config = PmemConfig::try_from(PmemConfigInput::new("pmem0", path_text(pmem.path())))
        .expect("bounded direct-pmem config should validate");
    let backing =
        PmemFileBacking::open(&pmem_config).expect("bounded direct-pmem backing should open");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("direct-pmem boot source should configure");
    let session_config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    )
    .with_pci_enabled();
    let mut backend = HvfBackend::new();
    let mut session = backend
        .prepare_arm64_boot_session(&controller, session_config)
        .expect("direct-pmem session should prepare");
    let before = process_memory_usage().expect("pre-insert process memory usage should read");
    session
        .insert_runtime_pmem_device(&pmem_config, backing)
        .expect("bounded direct-pmem device should map and publish");
    let after = process_memory_usage().expect("post-insert process memory usage should read");
    let growth = after.saturating_growth_from(before);

    assert!(
        growth.virtual_size <= PMEM_LEN + VIRTUAL_SIZE_SLACK,
        "one {PMEM_LEN}-byte direct pmem insertion must not add a second full-size virtual mapping; before {before:?}, after {after:?}, growth {growth:?}"
    );
    assert!(
        growth.resident_size <= PMEM_LEN + RESIDENT_SIZE_SLACK,
        "direct pmem insertion must keep resident growth within one backing plus generous framework slack; before {before:?}, after {after:?}, growth {growth:?}"
    );

    session
        .remove_runtime_pmem_device("pmem0")
        .expect("bounded direct-pmem device should flush and unmap");
    session
        .shutdown()
        .expect("bounded direct-pmem session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn guest_write_to_read_only_pmem_faults_without_mutating_backing() {
    use std::os::unix::fs::FileExt;

    use bangbang_hvf::{
        HvfArm64BootSessionConfig, HvfBackend, HvfVcpuExitResolveError, HvfVcpuRunnerError,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::{PmemConfigInput, PmemMmioLayout, VIRTIO_PMEM_ALIGNMENT};
    use bangbang_runtime::vsock::VsockMmioLayout;

    const WRITE_OFFSET: u64 = 4096;
    const WRITE_VALUE: u32 = 0xa5b6_c7d8;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("read-only-pmem-kernel", &image)
        .expect("temp read-only-pmem kernel should be created");
    let pmem = TempFile::new_len("read-only-pmem-backing", VIRTIO_PMEM_ALIGNMENT)
        .expect("temp read-only-pmem backing should be created");
    let observer =
        std::fs::File::open(pmem.path()).expect("independent read-only-pmem observer should open");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("read-only-pmem boot source should configure");
    controller
        .handle_action(VmmAction::PutPmem(
            PmemConfigInput::new("pmem0", path_text(pmem.path())).with_read_only(true),
        ))
        .expect("read-only-pmem device should configure");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut backend = HvfBackend::new();
    let mut session = backend
        .prepare_arm64_boot_session(&controller, config)
        .expect("read-only-pmem session should prepare");
    let boot_registers = session
        .boot_registers()
        .expect("read-only-pmem session should retain boot registers");
    let target = session.runtime_resources().pmem_devices[0]
        .guest_range()
        .start()
        .checked_add(WRITE_OFFSET)
        .expect("read-only-pmem guest target should fit");
    let program = arm64_store_u32_and_hvc_program(target.raw_value(), WRITE_VALUE);
    session
        .guest_memory_mut()
        .expect("read-only-pmem session should expose ordinary guest memory")
        .write_slice(&program, boot_registers.kernel_entry)
        .expect("read-only-pmem guest program should replace the test kernel entry");

    let err = session
        .run_once_and_handle_mmio()
        .expect_err("guest write to read-only pmem should fault before HVC");
    assert!(
        matches!(
            err,
            HvfVcpuRunnerError::VcpuExitResolve(HvfVcpuExitResolveError::MmioResolve { .. })
        ),
        "read-only pmem write should surface as an unowned write fault, got {err:?}"
    );

    let mut observed = [0_u8; std::mem::size_of::<u32>()];
    observer
        .read_exact_at(&mut observed, WRITE_OFFSET)
        .expect("independent observer should read after the rejected guest write");
    assert_eq!(
        observed,
        [0; std::mem::size_of::<u32>()],
        "a guest write fault must not mutate the read-only pmem backing"
    );

    session
        .shutdown()
        .expect("read-only-pmem session should shut down after the fault proof");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn prepares_owned_hvf_arm64_boot_session() {
    use bangbang_hvf::{
        ARM64_LINUX_BOOT_CPSR, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("owned-session-kernel", &image).expect("temp kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutMachineConfig(
            MachineConfigInput::new(1, 128).with_track_dirty_pages(true),
        ))
        .expect("tracked normal-boot machine config should be stored");
    let rtc_mmio_layout = test_rtc_mmio_layout();
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        rtc_mmio_layout,
    );

    let mut session = OwnedHvfArm64BootSession::new(&controller, config.clone())
        .expect("owned HVF arm64 boot session should prepare");

    let mmio_dispatcher = session.mmio_dispatcher();
    let mmio_regions = mmio_dispatcher
        .try_lock()
        .expect("owned session MMIO dispatcher should lock")
        .regions()
        .to_vec();
    assert_eq!(mmio_regions.len(), 1);
    assert_eq!(mmio_regions[0].id(), rtc_mmio_layout.region_id());
    assert_eq!(mmio_regions[0].range().start(), rtc_mmio_layout.base());
    assert_eq!(
        mmio_regions[0].range().size(),
        bangbang_runtime::rtc::RTC_MMIO_DEVICE_WINDOW_SIZE
    );
    assert!(session.block_interrupt_lines().is_empty());
    assert_eq!(
        session
            .guest_memory()
            .expect("owned session should expose mapped guest memory")
            .total_size(),
        session.runtime_resources().layout.total_size()
    );
    let dirty_tracker = session
        .guest_memory()
        .expect("tracked owned session should expose guest memory")
        .dirty_tracker()
        .expect("normal tracked startup should retain one dirty epoch");
    assert!(
        !dirty_tracker
            .dirty_pages()
            .expect("normal boot dirty pages should query")
            .is_empty(),
        "kernel, FDT, and device boot population must enter the initial epoch"
    );
    assert_eq!(session.reset_dirty_epoch_quiesced(), Ok(Some(1)));
    assert!(
        dirty_tracker
            .dirty_pages()
            .expect("reset normal-boot epoch should query")
            .is_empty()
    );
    let boot_origin = session
        .runtime_resources()
        .boot_origin
        .as_ref()
        .expect("ordinary session should retain boot-origin metadata");
    let boot_registers = session
        .boot_registers()
        .expect("ordinary session should retain boot registers");
    let mut fdt_magic = [0; 4];
    session
        .guest_memory()
        .expect("owned session should expose mapped guest memory")
        .read_slice(&mut fdt_magic, boot_origin.fdt.address)
        .expect("mapped guest memory should contain the written FDT");
    assert_eq!(u32::from_be_bytes(fdt_magic), 0xd00d_feed);
    assert_eq!(
        boot_registers.kernel_entry,
        boot_origin.loaded_boot_source.kernel.entry_address
    );
    assert_eq!(boot_registers.fdt_address, boot_origin.fdt.address);
    let register_state = session
        .capture_arm64_general_register_state()
        .expect("owned session should capture general-register state");
    assert_eq!(
        register_state.general_purpose_register(0),
        Some(boot_registers.fdt_address.raw_value())
    );
    assert_eq!(register_state.pc(), boot_registers.kernel_entry.raw_value());
    assert_eq!(register_state.cpsr(), ARM64_LINUX_BOOT_CPSR);
    session
        .restore_arm64_general_register_state(&register_state)
        .expect("owned session should restore general-register state");
    let core_system_register_state = session
        .capture_arm64_core_system_register_state()
        .expect("owned session should capture core system-register state");
    session
        .restore_arm64_core_system_register_state(&core_system_register_state)
        .expect("owned session should restore core system-register state");
    let exception_register_state = session
        .capture_arm64_exception_register_state()
        .expect("owned session should capture exception-register state");
    session
        .restore_arm64_exception_register_state(&exception_register_state)
        .expect("owned session should restore exception-register state");
    let execution_control_state = session
        .capture_arm64_execution_control_register_state()
        .expect("owned session should capture execution-control state");
    session
        .restore_arm64_execution_control_register_state(&execution_control_state)
        .expect("owned session should restore execution-control state");
    let cache_selection_state = session
        .capture_arm64_cache_selection_register_state()
        .expect("owned session should capture cache-selection state");
    session
        .restore_arm64_cache_selection_register_state(&cache_selection_state)
        .expect("owned session should restore cache-selection state");
    session
        .capture_arm64_breakpoint_register_state()
        .expect("owned session should capture breakpoint-register state");
    session
        .capture_arm64_watchpoint_register_state()
        .expect("owned session should capture watchpoint-register state");
    let debug_control_state = session
        .capture_arm64_debug_control_register_state()
        .expect("owned session should capture debug-control state");
    session
        .restore_arm64_debug_control_register_state(&debug_control_state)
        .expect("owned session should restore debug-control state");
    let debug_trap_state = session
        .capture_arm64_debug_trap_state()
        .expect("owned session should capture debug-trap state");
    session
        .restore_arm64_debug_trap_state(&debug_trap_state)
        .expect("owned session should restore debug-trap state");
    session
        .capture_arm64_identification_register_state()
        .expect("owned session should capture identification-register state");
    session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("owned session should capture SVE/SME identification state");
    let _sme_pstate =
        assert_sme_pstate_capture_supported_or_unavailable(session.capture_arm64_sme_pstate())
            .expect("owned session SME PSTATE capture should succeed or report unsupported");
    let _sme_p_registers = assert_sme_p_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_p_register_state(),
    )
    .expect("owned session SME P-register capture should succeed or report unavailable");
    let _sme_z_registers = assert_sme_z_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_z_register_state(),
    )
    .expect("owned session SME Z-register capture should succeed or report unavailable");
    let _sme_za_register = assert_sme_za_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_za_register_state(),
    )
    .expect("owned session SME ZA-register capture should succeed or report unavailable");
    let _sme_zt0_register = assert_sme_zt0_register_capture_supported_or_unavailable(
        session.capture_arm64_sme_zt0_register_state(),
    )
    .expect("owned session SME ZT0-register capture should succeed or report unavailable");
    session
        .capture_arm64_sme_system_register_state()
        .expect("owned session should capture SME system-register state");
    let system_context_state = session
        .capture_arm64_system_context_register_state()
        .expect("owned session should capture system-context register state");
    session
        .restore_arm64_system_context_register_state(&system_context_state)
        .expect("owned session should restore system-context register state");
    let translation_state = session
        .capture_arm64_translation_register_state()
        .expect("owned session should capture translation-register state");
    session
        .restore_arm64_translation_register_state(&translation_state)
        .expect("owned session should restore translation-register state");
    let pointer_authentication_key_state = session
        .capture_arm64_pointer_authentication_key_state()
        .expect("owned session should capture pointer-authentication key state");
    session
        .restore_arm64_pointer_authentication_key_state(&pointer_authentication_key_state)
        .expect("owned session should restore pointer-authentication key state");
    let thread_context_state = session
        .capture_arm64_thread_context_register_state()
        .expect("owned session should capture thread-context register state");
    session
        .restore_arm64_thread_context_register_state(&thread_context_state)
        .expect("owned session should restore thread-context register state");
    let simd_fp_state = session
        .capture_arm64_simd_fp_state()
        .expect("owned session should capture SIMD/FP state");
    session
        .restore_arm64_simd_fp_state(&simd_fp_state)
        .expect("owned session should restore SIMD/FP state");
    session
        .capture_arm64_physical_timer_state()
        .expect("owned session should capture physical-timer state");
    session
        .capture_arm64_virtual_timer_state()
        .expect("owned session should capture virtual-timer state");
    let snapshot_timer_state = session
        .capture_arm64_snapshot_timer_state()
        .expect("owned session should capture normalized timer state");
    let pending_interrupt_state = session
        .capture_arm64_pending_interrupt_state()
        .expect("owned session should capture pending-interrupt state");
    session
        .restore_arm64_pending_interrupt_state(&pending_interrupt_state)
        .expect("owned session should restore pending-interrupt state");
    let gic_device_state = session
        .capture_gic_device_state()
        .expect("owned session should capture GIC device state");
    assert!(!gic_device_state.is_empty());
    let gic_icc_register_state = session
        .capture_arm64_gic_icc_register_state()
        .expect("owned session should capture GIC ICC register state");
    session
        .restore_gic_device_state(&gic_device_state)
        .expect("owned session should restore GIC device state before run");
    session
        .restore_arm64_gic_icc_register_state(&gic_icc_register_state)
        .expect("owned session should restore GIC ICC register state before run");
    let restored_gic_icc_register_state = session
        .capture_arm64_gic_icc_register_state()
        .expect("owned session should capture GIC ICC register state");
    assert!(
        restored_gic_icc_register_state == gic_icc_register_state,
        "owned session should preserve original GIC ICC register state"
    );
    session
        .restore_arm64_snapshot_timer_state(snapshot_timer_state)
        .expect("owned session should restore normalized timers after GIC state");
    assert_normalized_timer_restore_equivalent(
        snapshot_timer_state,
        session
            .capture_arm64_snapshot_timer_state()
            .expect("owned session should recapture normalized timers"),
    );
    let old_vmgenid = session.runtime_resources().vmgenid_device;
    session
        .replace_vmgenid_for_snapshot_restore()
        .expect("owned session should replace VMGenID and inject its SPI");
    let new_vmgenid = session.runtime_resources().vmgenid_device;
    assert_ne!(new_vmgenid.generation_id, old_vmgenid.generation_id);
    assert_eq!(new_vmgenid.range, old_vmgenid.range);
    assert_eq!(new_vmgenid.fdt_device, old_vmgenid.fdt_device);
    let mut guest_vmgenid = [0; bangbang_runtime::startup::ARM64_BOOT_VMGENID_SIZE];
    session
        .guest_memory()
        .expect("owned session should expose VMGenID guest memory")
        .read_slice(&mut guest_vmgenid, new_vmgenid.range.start())
        .expect("owned session replacement VMGenID should read");
    assert_eq!(guest_vmgenid, new_vmgenid.generation_id);
    let page_size = host_page_size().expect("host page size should remain available");
    let vmgenid_page = GuestAddress::new(new_vmgenid.range.start().raw_value() & !(page_size - 1));
    assert_eq!(
        dirty_tracker
            .dirty_pages()
            .expect("VMGenID device dirty page should query"),
        vec![vmgenid_page]
    );
    assert_eq!(session.reset_dirty_epoch_quiesced(), Ok(Some(2)));
    let run_cancel_handle = session.run_cancel_handle();
    drop(run_cancel_handle);
    let run_loop_control = session.run_loop_control();
    let run_loop_stop_token = run_loop_control.stop_token();
    run_loop_control
        .request_stop()
        .expect("owned HVF boot-session run-loop stop should request vCPU cancellation");
    assert!(run_loop_stop_token.is_stop_requested());
    session
        .shutdown()
        .expect("owned HVF arm64 boot session should shut down");
    session
        .shutdown()
        .expect("repeated owned HVF arm64 boot session shutdown should be idempotent");
    drop(session);

    let mut second_session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("second owned HVF arm64 boot session should prepare after shutdown");
    assert_eq!(
        second_session
            .guest_memory_mut()
            .expect("second owned session should expose mutable mapped guest memory")
            .total_size(),
        second_session.runtime_resources().layout.total_size()
    );
    second_session
        .shutdown()
        .expect("second owned HVF arm64 boot session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn native_v2_three_vcpu_platform_round_trip_preserves_paused_lifecycle_and_progress() {
    use std::fs::{File, OpenOptions};
    use std::io::{Read, Seek, SeekFrom};
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use bangbang_hvf::{
        HvfArm64BootRunLoopOutcome, HvfArm64BootSessionConfig, HvfArm64BootSnapshotV2CaptureError,
        HvfArm64BootSnapshotV2CaptureInput, HvfArm64StableVcpuDisposition, HvfSnapshotV2BootState,
        HvfSnapshotV2NativePath, HvfVcpuRunStepOutcome, OwnedHvfArm64BootSession,
        decode_hvf_snapshot_v2_platform_state, encode_hvf_snapshot_v2_platform_state,
        restore_hvf_snapshot_v2_platform,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::snapshot_device_v2::NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION;
    use bangbang_runtime::snapshot_format_v2::{
        NATIVE_V2_LEGACY_PLATFORM_VERSION, decode_snapshot_v2_state,
    };
    use bangbang_runtime::snapshot_memory_v2::{
        NATIVE_V2_MEMORY_HEADER_BYTES, SnapshotV2MemoryIoStage, SnapshotV2MemoryWriteError,
        load_snapshot_v2_memory_file,
    };
    use bangbang_runtime::startup::ARM64_BOOT_VMGENID_SIZE;
    use bangbang_runtime::vmclock::{VMCLOCK_ABI_SIZE, VmClockAbi};
    use bangbang_runtime::vsock::VsockMmioLayout;

    const SECONDARY_ONE_OFFSET: u64 = 0x1000;
    const SECONDARY_TWO_OFFSET: u64 = 0x2000;
    const VECTOR_OFFSET: u64 = 0x3000;
    const IRQ_HANDLER_OFFSET: u64 = VECTOR_OFFSET + 0x280;
    const FLAGS_OFFSET: u64 = 0x4000;
    const FLAGS_SIZE: usize = 0x80;
    const CONFIG_OFFSET: u64 = 0x5000;
    const CPU_ON_ONE_RESULT: usize = 0x00;
    const PRIMARY_BEFORE_CAPTURE: usize = 0x08;
    const PRE_SUSPEND: usize = 0x0c;
    const POST_SUSPEND: usize = 0x10;
    const SUSPEND_RESULT: usize = 0x18;
    const SUSPEND_SENTINEL: usize = 0x20;
    const PRIMARY_AFTER_RESTORE: usize = 0x28;
    const CPU_ON_TWO_RESULT: usize = 0x30;
    const CPU_TWO_PROGRESS: usize = 0x38;
    const FINAL_CHECKPOINT: usize = 0x3c;
    const IDENTITY_ACK_COUNT: usize = 0x40;
    const VMGENID_ACK: usize = 0x44;
    const VMCLOCK_ACK: usize = 0x48;
    const SOURCE_RTC_SECONDS: usize = 0x4c;
    const GUEST_VMGENID_WORD: usize = 0x50;
    const GUEST_VMCLOCK_SEQUENCE: usize = 0x58;
    const GUEST_VMCLOCK_GENERATION: usize = 0x60;
    const GUEST_RTC_SECONDS: usize = 0x68;
    const GUEST_PVTIME_IPA: usize = 0x70;
    const GUEST_PVTIME_STOLEN_NS: usize = 0x78;
    const SENTINEL: u64 = 0x5a5a;
    const SOURCE_RTC_SENTINEL: u32 = 0x1234;
    const IDENTITY_ACK_FUNCTION: u64 = 0x7ffe;
    const PSCI_VERSION: u64 = 0x8400_0000;
    const PSCI_CPU_SUSPEND_64: u64 = 0xc400_0001;
    const PSCI_CPU_ON_64: u64 = 0xc400_0003;
    const ARM_SMCCC_PV_TIME_ST_64: u64 = 0xc500_0021;

    let primary_code = arm64_instruction_bytes(&[
        arm64_adr(0, FLAGS_OFFSET, 19),            // adr x19, flags
        arm64_adr(4, CONFIG_OFFSET, 18),           // adr x18, config
        0xf940_0254,                               // ldr x20, [x18] (GICD)
        0xf940_0655,                               // ldr x21, [x18, #8] (GICR)
        0xf940_0e57,                               // ldr x23, [x18, #24] (VBAR)
        0xd518_c017,                               // msr VBAR_EL1, x23
        0xd503_3fdf,                               // isb
        0x9100_52a1,                               // add x1, x21, #0x14 (GICR_WAKER)
        0xb940_0022,                               // ldr w2, [x1]
        0x121e_7842,                               // bic w2, w2, #2 (ProcessorSleep)
        0xb900_0022,                               // str w2, [x1]
        0xb940_0022,                               // ldr w2, [x1]
        0x3717_ffe2, // tbnz w2, #2, previous instruction (ChildrenAsleep)
        0xb940_1256, // ldr w22, [x18, #16] (VMGenID INTID)
        0x5280_1007, // mov w7, #0x80 (higher priority)
        0x9400_004f, // bl configure_spi
        0xb940_1656, // ldr w22, [x18, #20] (VMClock INTID)
        0x5280_1207, // mov w7, #0x90
        0x9400_004c, // bl configure_spi
        0xb940_0287, // ldr w7, [x20] (GICD_CTLR)
        0x5280_0248, // mov w8, #0x12 (ARE_NS | EnableGrp1NS)
        0x2a08_00e7, // orr w7, w7, w8
        0xb900_0287, // str w7, [x20]
        0xd503_3f9f, // dsb sy
        0xb940_0287, // ldr w7, [x20]
        0x37ff_ffe7, // tbnz w7, #31, previous instruction (RWP)
        0xd538_cca1, // mrs x1, ICC_SRE_EL1
        0xb240_0021, // orr x1, x1, #1
        0xd518_cca1, // msr ICC_SRE_EL1, x1
        0xd503_3fdf, // isb
        0xd280_1fe1, // mov x1, #0xff
        0xd518_4601, // msr ICC_PMR_EL1, x1
        0xd518_cc7f, // msr ICC_BPR1_EL1, xzr
        0xd280_0021, // mov x1, #1
        0xd518_cce1, // msr ICC_IGRPEN1_EL1, x1
        0xd503_3fdf, // isb
        0xd503_42ff, // msr DAIFClr, #2
        0xf940_1a48, // ldr x8, [x18, #48] (PL031)
        0x5282_4689, // mov w9, #0x1234
        0xb900_0909, // str w9, [x8, #8] (RTC_LOAD)
        0xb940_0109, // ldr w9, [x8] (RTC_DR)
        0xb900_4e69, // str w9, [x19, #0x4c]
        0xd280_0060, // mov x0, #3
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (CPU_ON64)
        0xd280_0021, // mov x1, #1
        arm64_adr(0xb4, SECONDARY_ONE_OFFSET, 2), // adr x2, secondary one
        arm64_adr(0xb8, FLAGS_OFFSET, 3), // adr x3, flags
        0xd400_0002, // hvc #0
        0xf900_0260, // str x0, [x19]
        0xb940_0e64, // ldr w4, [x19, #0xc]
        0x34ff_ffe4, // cbz w4, previous instruction
        0x5280_0024, // mov w4, #1
        0xb900_0a64, // str w4, [x19, #8]
        0xd280_0000, // mov x0, #0
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (PSCI_VERSION)
        0xd400_0002, // hvc #0 (source checkpoint)
        0xb940_4267, // ldr w7, [x19, #0x40]
        0x7100_08ff, // cmp w7, #2
        0x54ff_ffc1, // b.ne to acknowledgement count load
        0xf940_1248, // ldr x8, [x18, #32] (VMGenID)
        0xf940_0109, // ldr x9, [x8]
        0xf900_2a69, // str x9, [x19, #0x50]
        0xf940_1648, // ldr x8, [x18, #40] (VMClock)
        0xb940_0d09, // ldr w9, [x8, #12] (sequence)
        0xb900_5a69, // str w9, [x19, #0x58]
        0xf940_3509, // ldr x9, [x8, #104] (generation)
        0xf900_3269, // str x9, [x19, #0x60]
        0xf940_1a48, // ldr x8, [x18, #48] (PL031)
        0xb940_0109, // ldr w9, [x8] (RTC_DR)
        0xb900_6a69, // str w9, [x19, #0x68]
        0xd280_0420, // mov x0, #0x21
        0xf2b8_a000, // movk x0, #0xc500, lsl #16 (PV_TIME_ST64)
        0xd400_0002, // hvc #0
        0xf900_3a60, // str x0, [x19, #0x70]
        0xf940_0409, // ldr x9, [x0, #8] (stolen time)
        0xf900_3e69, // str x9, [x19, #0x78]
        0x5280_0024, // mov w4, #1
        0xb900_2a64, // str w4, [x19, #0x28]
        0xb940_1265, // ldr w5, [x19, #0x10]
        0x34ff_ffe5, // cbz w5, previous instruction
        0xd280_0060, // mov x0, #3
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (CPU_ON64)
        0xd280_0041, // mov x1, #2
        arm64_adr(0x14c, SECONDARY_TWO_OFFSET, 2), // adr x2, secondary two
        arm64_adr(0x150, FLAGS_OFFSET, 3), // adr x3, flags
        0xd400_0002, // hvc #0
        0xf900_1a60, // str x0, [x19, #0x30]
        0xb940_3a66, // ldr w6, [x19, #0x38]
        0x34ff_ffe6, // cbz w6, previous instruction
        0xb900_3e64, // str w4, [x19, #0x3c]
        0xd280_0000, // mov x0, #0
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (PSCI_VERSION)
        0xd400_0002, // hvc #0 (final checkpoint)
        0x1400_0000, // b .
        0x1200_12c3, // configure_spi: and w3, w22, #31
        0x5280_0024, // mov w4, #1
        0x1ac3_2084, // lsl w4, w4, w3
        0x5305_7ec5, // lsr w5, w22, #5
        0x9102_0286, // add x6, x20, #0x80 (GICD_IGROUPR)
        0x8b05_08c6, // add x6, x6, x5, lsl #2
        0xb940_00c9, // ldr w9, [x6]
        0x2a04_0129, // orr w9, w9, w4
        0xb900_00c9, // str w9, [x6]
        0x1200_0ec3, // and w3, w22, #15
        0x531f_7863, // lsl w3, w3, #1
        0x1100_0463, // add w3, w3, #1
        0x5280_0024, // mov w4, #1
        0x1ac3_2084, // lsl w4, w4, w3
        0x9130_0286, // add x6, x20, #0xc00 (GICD_ICFGR)
        0x5304_7ec5, // lsr w5, w22, #4
        0x8b05_08c6, // add x6, x6, x5, lsl #2
        0xb940_00c9, // ldr w9, [x6]
        0x2a04_0129, // orr w9, w9, w4
        0xb900_00c9, // str w9, [x6]
        0x9110_0286, // add x6, x20, #0x400 (GICD_IPRIORITYR)
        0x8b16_00c6, // add x6, x6, x22
        0x3900_00c7, // strb w7, [x6]
        0x9140_1a86, // add x6, x20, #0x6000 (GICD_IROUTER)
        0x8b16_0cc6, // add x6, x6, x22, lsl #3
        0xf900_00df, // str xzr, [x6]
        0x1200_12c3, // and w3, w22, #31
        0x5280_0024, // mov w4, #1
        0x1ac3_2084, // lsl w4, w4, w3
        0x5305_7ec5, // lsr w5, w22, #5
        0x9104_0286, // add x6, x20, #0x100 (GICD_ISENABLER)
        0x8b05_08c6, // add x6, x6, x5, lsl #2
        0xb900_00c4, // str w4, [x6]
        0xd65f_03c0, // ret
    ]);
    let irq_code = arm64_instruction_bytes(&[
        0xd538_cc00, // mrs x0, ICC_IAR1_EL1
        0xb940_4261, // ldr w1, [x19, #0x40]
        0x9101_1262, // add x2, x19, #0x44
        0xb821_7840, // str w0, [x2, x1, lsl #2]
        0x1100_0421, // add w1, w1, #1
        0xb900_4261, // str w1, [x19, #0x40]
        0xd518_cc20, // msr ICC_EOIR1_EL1, x0
        0xd28f_ffc0, // mov x0, #0x7ffe
        0xd400_0002, // hvc #0
        0xd69f_03e0, // eret
    ]);
    let secondary_one_code = arm64_instruction_bytes(&[
        0xaa00_03f3, // mov x19, x0
        0xd28b_4b54, // mov x20, #0x5a5a
        0xd53b_e044, // mrs x4, CNTVCT_EL0
        0xd53b_e005, // mrs x5, CNTFRQ_EL0
        0x8b05_0084, // add x4, x4, x5
        0xd51b_e344, // msr CNTV_CVAL_EL0, x4
        0xd280_0024, // mov x4, #1
        0xd51b_e324, // msr CNTV_CTL_EL0, x4
        0xd503_3fdf, // isb
        0x5280_0026, // mov w6, #1
        0xb900_0e66, // str w6, [x19, #0xc]
        0xd280_0020, // mov x0, #1
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (CPU_SUSPEND64)
        0xd280_0001, // mov x1, #0
        0xd280_0002, // mov x2, #0
        0xd280_0003, // mov x3, #0
        0xd400_0002, // hvc #0
        0xf900_0e60, // str x0, [x19, #0x18]
        0xf900_1274, // str x20, [x19, #0x20]
        0xb900_1266, // str w6, [x19, #0x10]
        0xd280_0040, // mov x0, #2
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (CPU_OFF)
        0xd400_0002, // hvc #0
        0x1400_0000, // b .
    ]);
    let secondary_two_code = arm64_instruction_bytes(&[
        0xaa00_03f3, // mov x19, x0
        0x5280_0026, // mov w6, #1
        0xb900_3a66, // str w6, [x19, #0x38]
        0xd280_0040, // mov x0, #2
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (CPU_OFF)
        0xd400_0002, // hvc #0
        0x1400_0000, // b .
    ]);

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("native-v2-platform-kernel", &image).expect("temp kernel should create");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source should configure");
    controller
        .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(3, 16)))
        .expect("three-vCPU machine should configure");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut source = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("native-v2 source should prepare");
    let primary_entry = GuestAddress::new(
        source
            .capture_arm64_general_register_state()
            .expect("primary entry should capture")
            .pc(),
    );
    let secondary_one_entry = primary_entry
        .checked_add(SECONDARY_ONE_OFFSET)
        .expect("secondary-one entry should fit");
    let secondary_two_entry = primary_entry
        .checked_add(SECONDARY_TWO_OFFSET)
        .expect("secondary-two entry should fit");
    let vector_base = primary_entry
        .checked_add(VECTOR_OFFSET)
        .expect("exception-vector base should fit");
    let irq_handler = primary_entry
        .checked_add(IRQ_HANDLER_OFFSET)
        .expect("IRQ handler should fit");
    let flags = primary_entry
        .checked_add(FLAGS_OFFSET)
        .expect("shared flags should fit");
    let guest_config_address = primary_entry
        .checked_add(CONFIG_OFFSET)
        .expect("guest evidence configuration should fit");
    let gic = source.gic_metadata();
    let vmgenid = source.runtime_resources().vmgenid_device;
    let vmclock = source.runtime_resources().vmclock_device;
    let vmgenid_intid = vmgenid.fdt_device.interrupt_line.raw_value();
    let vmclock_intid = vmclock.fdt_device.interrupt_line.raw_value();
    assert_ne!(
        vmgenid_intid, vmclock_intid,
        "identity notification lines must remain distinct"
    );
    let mut guest_config = Vec::with_capacity(56);
    guest_config.extend_from_slice(&gic.distributor.base.to_le_bytes());
    guest_config.extend_from_slice(&gic.redistributor.region.base.to_le_bytes());
    guest_config.extend_from_slice(&vmgenid_intid.to_le_bytes());
    guest_config.extend_from_slice(&vmclock_intid.to_le_bytes());
    guest_config.extend_from_slice(&vector_base.raw_value().to_le_bytes());
    guest_config.extend_from_slice(&vmgenid.range.start().raw_value().to_le_bytes());
    guest_config.extend_from_slice(&vmclock.range.start().raw_value().to_le_bytes());
    guest_config.extend_from_slice(&test_rtc_mmio_layout().base().raw_value().to_le_bytes());
    assert_eq!(guest_config.len(), 56);
    {
        let memory = source
            .guest_memory_mut()
            .expect("source memory should be mutable before execution");
        memory
            .write_slice(&primary_code, primary_entry)
            .expect("primary code should fit");
        memory
            .write_slice(&secondary_one_code, secondary_one_entry)
            .expect("secondary-one code should fit");
        memory
            .write_slice(&secondary_two_code, secondary_two_entry)
            .expect("secondary-two code should fit");
        memory
            .write_slice(&irq_code, irq_handler)
            .expect("identity IRQ handler should fit");
        memory
            .write_slice(&[0; FLAGS_SIZE], flags)
            .expect("shared flags should fit");
        memory
            .write_slice(&guest_config, guest_config_address)
            .expect("guest evidence configuration should fit");
    }
    let source_flags_host = {
        let memory = source
            .guest_memory()
            .expect("source memory should remain mapped");
        let region = memory
            .regions()
            .iter()
            .find(|region| region.range().contains(flags))
            .expect("source flags should belong to mapped DRAM");
        let offset = flags
            .raw_value()
            .checked_sub(region.range().start().raw_value())
            .and_then(|offset| usize::try_from(offset).ok())
            .expect("source flag host offset should fit");
        region.host_address().as_ptr().cast::<u8>() as usize + offset
    };
    let read_source_u32 = |offset: usize| {
        // SAFETY: each aligned address remains inside the mapped shared flag
        // area while `source` owns its guest memory; volatile reads observe
        // stores from concurrently executing guest vCPUs.
        unsafe { std::ptr::read_volatile((source_flags_host + offset) as *const u32) }
    };
    let read_source_u64 = |offset: usize| {
        // SAFETY: the same owned mapping and alignment argument as above
        // applies to each eight-byte field.
        unsafe { std::ptr::read_volatile((source_flags_host + offset) as *const u64) }
    };

    let source_control = source.run_loop_control();
    let source_stop = source_control.stop_token();
    let one_step = NonZeroUsize::new(1).expect("one is nonzero");
    let mut source_checkpoint = false;
    let mut source_suspend = false;
    for _ in 0..24 {
        let mut observed = None;
        let outcome = source
            .run_loop_with_observer(&source_stop, one_step, |step| observed = Some(*step))
            .expect("source step should succeed");
        assert!(matches!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        ));
        match observed.expect("one source step should be observed") {
            HvfVcpuRunStepOutcome::CpuSuspend {
                index: 1,
                function_id: PSCI_CPU_SUSPEND_64,
                ..
            } => source_suspend = true,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_VERSION,
                return_value: 0x0001_0000,
                ..
            } => {
                source_checkpoint = true;
                break;
            }
            _ => {}
        }
    }
    assert!(source_suspend, "secondary one should enter CPU_SUSPEND");
    assert!(
        source_checkpoint,
        "primary should reach the source checkpoint"
    );
    assert_eq!(read_source_u64(CPU_ON_ONE_RESULT), 0);
    assert_eq!(read_source_u32(PRIMARY_BEFORE_CAPTURE), 1);
    assert_eq!(read_source_u32(PRE_SUSPEND), 1);
    assert_eq!(read_source_u32(POST_SUSPEND), 0);
    assert_eq!(read_source_u32(PRIMARY_AFTER_RESTORE), 0);
    assert_eq!(read_source_u32(CPU_TWO_PROGRESS), 0);
    assert_eq!(read_source_u32(IDENTITY_ACK_COUNT), 0);
    assert!(
        read_source_u32(SOURCE_RTC_SECONDS).wrapping_sub(SOURCE_RTC_SENTINEL) <= 5,
        "source guest should observe its deliberately mutated PL031 value"
    );

    source
        .pause_for_snapshot_v2_capture()
        .expect("source should complete a snapshot-v2 pause without redispatch");

    let boot = HvfSnapshotV2BootState::try_new(
        HvfSnapshotV2NativePath::try_new(kernel.path().as_os_str())
            .expect("kernel metadata path should validate"),
        None,
        None,
    )
    .expect("native-v2 boot metadata should validate");
    let capture_input = HvfArm64BootSnapshotV2CaptureInput::new(boot);
    let nonempty_artifact = TempFile::new_len("native-v2-platform-nonempty-memory", 1)
        .expect("nonempty memory artifact should create");
    let mut nonempty_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(nonempty_artifact.path())
        .expect("nonempty memory artifact should open");
    let capture_error = source
        .capture_snapshot_v2_platform(capture_input.clone(), &mut nonempty_writer)
        .expect_err("nonempty memory output should reject after stable owner capture");
    assert!(
        matches!(
            capture_error,
            HvfArm64BootSnapshotV2CaptureError::MemoryImage {
                source: SnapshotV2MemoryWriteError::NonEmptyOutput
            }
        ),
        "unexpected capture error: {capture_error:?}"
    );
    let cancelled_artifact = TempFile::new_len("native-v2-platform-cancelled-memory", 0)
        .expect("cancelled memory artifact should create");
    let mut cancelled_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(cancelled_artifact.path())
        .expect("cancelled memory artifact should open");
    let capture_error = source
        .capture_snapshot_v2_platform_with_cancel(
            capture_input.clone(),
            &mut cancelled_writer,
            |stage| matches!(stage, SnapshotV2MemoryIoStage::Data { extent_index: 0 }),
        )
        .expect_err("bounded memory cancellation should abort platform capture");
    assert!(
        matches!(
            capture_error,
            HvfArm64BootSnapshotV2CaptureError::MemoryImage {
                source: SnapshotV2MemoryWriteError::Cancelled {
                    stage: SnapshotV2MemoryIoStage::Data { extent_index: 0 }
                }
            }
        ),
        "unexpected cancelled capture error: {capture_error:?}"
    );
    drop(cancelled_writer);
    source
        .resume_after_snapshot_v2_capture()
        .expect("cancelled source coordinator should recover");
    source
        .pause_for_snapshot_v2_capture()
        .expect("recovered cancelled source should pause again");
    let exact_memory_artifact = TempFile::new_len("native-v2-platform-exact-2-4-memory", 0)
        .expect("exact 2.4 memory artifact should create");
    let mut exact_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(exact_memory_artifact.path())
        .expect("exact 2.4 memory artifact should open");
    let exact_capture = source
        .capture_snapshot_v2_device_graph_platform_with_cancel(
            capture_input.clone(),
            &mut exact_writer,
            |_| false,
        )
        .expect("exact 2.4 platform capture should succeed");
    assert_eq!(
        exact_capture.memory().version(),
        NATIVE_V2_DEVICE_GRAPH_COMPATIBILITY_VERSION
    );
    assert_eq!(
        exact_writer
            .metadata()
            .expect("exact 2.4 memory metadata should read")
            .len(),
        exact_capture.memory().file_length()
    );
    let encoded_binding = exact_capture
        .memory()
        .encode()
        .expect("exact 2.4 memory binding should encode");
    let mut image_header = [0_u8; NATIVE_V2_MEMORY_HEADER_BYTES];
    exact_writer
        .seek(SeekFrom::Start(0))
        .expect("exact 2.4 memory image should rewind");
    exact_writer
        .read_exact(&mut image_header)
        .expect("exact 2.4 memory image header should read");
    assert_eq!(
        image_header.as_slice(),
        &encoded_binding[..NATIVE_V2_MEMORY_HEADER_BYTES]
    );
    let exact_debug = format!("{exact_capture:?}");
    assert!(!exact_debug.contains(&path_text(kernel.path())));
    drop(exact_writer);
    source
        .resume_after_snapshot_v2_capture()
        .expect("exact 2.4 source coordinator should recover");
    source
        .pause_for_snapshot_v2_capture()
        .expect("source should pause again after exact 2.4 capture");
    let first_memory_artifact = TempFile::new_len("native-v2-platform-first-memory", 0)
        .expect("first memory artifact should create");
    let mut first_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(first_memory_artifact.path())
        .expect("first memory artifact should open");
    let first_capture = source
        .capture_snapshot_v2_platform(capture_input.clone(), &mut first_writer)
        .expect("first paused platform capture should succeed");
    assert_eq!(
        first_capture.memory().version(),
        NATIVE_V2_LEGACY_PLATFORM_VERSION
    );
    drop(first_writer);
    source
        .resume_after_snapshot_v2_capture()
        .expect("source coordinator should recover without outer guest dispatch");
    source
        .pause_for_snapshot_v2_capture()
        .expect("recovered source should complete a fresh snapshot-v2 pause");
    let memory_artifact =
        TempFile::new_len("native-v2-platform-memory", 0).expect("memory artifact should create");
    let mut writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(memory_artifact.path())
        .expect("memory artifact should open for writing");
    let second_capture = source
        .capture_snapshot_v2_platform_with_cancel(capture_input, &mut writer, |_| false)
        .expect("recovered source should support cancellable recapture");
    drop(writer);
    assert_native_v2_platform_recapture_equivalent(&first_capture, &second_capture);
    assert_eq!(first_capture.global(), second_capture.global());
    assert_eq!(first_capture.time(), second_capture.time());
    assert!(matches!(
        second_capture.topology().members()[0].disposition(),
        HvfArm64StableVcpuDisposition::Runnable
    ));
    assert!(matches!(
        second_capture.topology().members()[1].disposition(),
        HvfArm64StableVcpuDisposition::Suspended(_)
    ));
    assert!(matches!(
        second_capture.topology().members()[2].disposition(),
        HvfArm64StableVcpuDisposition::Offline
    ));
    let virtual_timer_intid = second_capture.topology().virtual_timer_intid();
    let mut source_vmgenid = [0; ARM64_BOOT_VMGENID_SIZE];
    source
        .guest_memory()
        .expect("source memory should remain mapped")
        .read_slice(
            &mut source_vmgenid,
            second_capture.time().vmgenid().range().start(),
        )
        .expect("source VMGenID should read");
    assert!(source_vmgenid.iter().any(|byte| *byte != 0));
    let source_vmclock = second_capture.time().vmclock_abi();
    let source_pvtime = second_capture.time().pvtime_vcpus().to_vec();

    let encoded = encode_hvf_snapshot_v2_platform_state(&second_capture)
        .expect("complete platform should encode");
    let structural =
        decode_snapshot_v2_state(&encoded).expect("native-v2 container should decode first");
    let decoded = decode_hvf_snapshot_v2_platform_state(&structural)
        .expect("typed platform should decode and cross-validate");
    assert_native_v2_platform_recapture_equivalent(&second_capture, &decoded);
    assert_eq!(decoded.global(), second_capture.global());
    assert_eq!(decoded.time(), second_capture.time());
    let assert_vmclock_transition = |saved: VmClockAbi, destination: VmClockAbi| {
        assert_eq!(
            destination.sequence(),
            (saved.sequence() | 1).wrapping_add(1)
        );
        assert_eq!(
            destination.disruption_marker(),
            saved.disruption_marker().wrapping_add(1)
        );
        assert_eq!(
            destination.generation_counter(),
            saved.generation_counter().wrapping_add(1)
        );
    };
    let first_clone_memory = load_snapshot_v2_memory_file(
        &structural,
        File::open(memory_artifact.path()).expect("memory artifact should open read-only"),
    )
    .expect("validated memory artifact should load");

    source
        .shutdown()
        .expect("paused source should shut down cleanly");
    drop(source);

    let mut first_clone = restore_hvf_snapshot_v2_platform(decoded.clone(), first_clone_memory)
        .expect("first immutable native-v2 clone should restore");
    assert_eq!(first_clone.vcpu_count(), 3);
    assert_eq!(first_clone.vcpu_mpidrs(), [0, 1, 2]);
    let mut first_clone_vmgenid = [0; ARM64_BOOT_VMGENID_SIZE];
    first_clone
        .guest_memory()
        .expect("first clone memory should remain mapped")
        .read_slice(
            &mut first_clone_vmgenid,
            second_capture.time().vmgenid().range().start(),
        )
        .expect("first clone VMGenID should read");
    assert!(first_clone_vmgenid.iter().any(|byte| *byte != 0));
    assert_ne!(first_clone_vmgenid, source_vmgenid);

    let first_clone_memory_artifact = TempFile::new_len("native-v2-platform-first-clone-memory", 0)
        .expect("first-clone memory artifact should create");
    let mut first_clone_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(first_clone_memory_artifact.path())
        .expect("first-clone memory artifact should open");
    let immediate_recapture = first_clone
        .capture_snapshot_v2_platform(&mut first_clone_writer)
        .expect("first clone should remain paused and recapturable");
    drop(first_clone_writer);
    assert_native_v2_platform_recapture_equivalent(&second_capture, &immediate_recapture);
    assert_eq!(
        immediate_recapture.time().rtc_layout(),
        second_capture.time().rtc_layout()
    );
    assert_eq!(
        immediate_recapture.time().vmgenid(),
        second_capture.time().vmgenid()
    );
    assert_eq!(
        immediate_recapture.time().vmclock(),
        second_capture.time().vmclock()
    );
    assert_eq!(
        immediate_recapture.time().pvtime_vcpus(),
        source_pvtime.as_slice(),
        "snapshot downtime must not be charged as PVTime stolen time"
    );
    assert_vmclock_transition(source_vmclock, immediate_recapture.time().vmclock_abi());

    let recaptured_encoded = encode_hvf_snapshot_v2_platform_state(&immediate_recapture)
        .expect("recaptured first clone should encode");
    let recaptured_structural = decode_snapshot_v2_state(&recaptured_encoded)
        .expect("recaptured first-clone container should decode");
    let recaptured_decoded = decode_hvf_snapshot_v2_platform_state(&recaptured_structural)
        .expect("recaptured first clone should cross-validate");
    assert_eq!(recaptured_decoded, immediate_recapture);
    first_clone
        .shutdown()
        .expect("first immutable clone should shut down cleanly");
    drop(first_clone);

    let second_clone_memory = load_snapshot_v2_memory_file(
        &structural,
        File::open(memory_artifact.path()).expect("memory artifact should reopen read-only"),
    )
    .expect("same immutable memory artifact should load again");
    let mut second_clone = restore_hvf_snapshot_v2_platform(decoded.clone(), second_clone_memory)
        .expect("second immutable native-v2 clone should restore");
    let mut second_clone_vmgenid = [0; ARM64_BOOT_VMGENID_SIZE];
    let mut second_clone_vmclock_bytes = [0; VMCLOCK_ABI_SIZE];
    {
        let memory = second_clone
            .guest_memory()
            .expect("second clone memory should remain mapped");
        memory
            .read_slice(
                &mut second_clone_vmgenid,
                second_capture.time().vmgenid().range().start(),
            )
            .expect("second clone VMGenID should read");
        memory
            .read_slice(
                &mut second_clone_vmclock_bytes,
                second_capture.time().vmclock().range().start(),
            )
            .expect("second clone VMClock should read");
    }
    assert!(second_clone_vmgenid.iter().any(|byte| *byte != 0));
    assert_ne!(second_clone_vmgenid, source_vmgenid);
    assert_ne!(
        second_clone_vmgenid, first_clone_vmgenid,
        "repeated loads of one immutable image need distinct clone identities"
    );
    let second_clone_vmclock = VmClockAbi::from_bytes(second_clone_vmclock_bytes)
        .expect("second clone VMClock should remain valid");
    assert_vmclock_transition(source_vmclock, second_clone_vmclock);
    second_clone
        .shutdown()
        .expect("second immutable clone should shut down cleanly");
    drop(second_clone);

    let recaptured_memory = load_snapshot_v2_memory_file(
        &recaptured_structural,
        File::open(first_clone_memory_artifact.path())
            .expect("first-clone memory artifact should open read-only"),
    )
    .expect("recaptured first-clone memory should load");
    let mut recaptured_clone =
        restore_hvf_snapshot_v2_platform(recaptured_decoded, recaptured_memory)
            .expect("recaptured paused clone should restore again");
    assert_eq!(recaptured_clone.vcpu_count(), 3);
    assert_eq!(recaptured_clone.vcpu_mpidrs(), [0, 1, 2]);
    let mut recaptured_clone_vmgenid = [0; ARM64_BOOT_VMGENID_SIZE];
    let mut recaptured_clone_vmclock_bytes = [0; VMCLOCK_ABI_SIZE];
    {
        let memory = recaptured_clone
            .guest_memory()
            .expect("restored recapture memory should remain mapped");
        memory
            .read_slice(
                &mut recaptured_clone_vmgenid,
                immediate_recapture.time().vmgenid().range().start(),
            )
            .expect("restored recapture VMGenID should read");
        memory
            .read_slice(
                &mut recaptured_clone_vmclock_bytes,
                immediate_recapture.time().vmclock().range().start(),
            )
            .expect("restored recapture VMClock should read");
    }
    assert!(recaptured_clone_vmgenid.iter().any(|byte| *byte != 0));
    assert_ne!(recaptured_clone_vmgenid, source_vmgenid);
    assert_ne!(recaptured_clone_vmgenid, first_clone_vmgenid);
    assert_ne!(recaptured_clone_vmgenid, second_clone_vmgenid);
    let recaptured_clone_vmclock = VmClockAbi::from_bytes(recaptured_clone_vmclock_bytes)
        .expect("restored recapture VMClock should remain valid");
    assert_vmclock_transition(
        immediate_recapture.time().vmclock_abi(),
        recaptured_clone_vmclock,
    );
    recaptured_clone
        .shutdown()
        .expect("recaptured clone should shut down cleanly");
    drop(recaptured_clone);

    let restored_memory = load_snapshot_v2_memory_file(
        &structural,
        File::open(memory_artifact.path()).expect("original memory artifact should reopen"),
    )
    .expect("original immutable memory artifact should load for guest evidence");
    let mut restored = restore_hvf_snapshot_v2_platform(decoded, restored_memory)
        .expect("clean original native-v2 clone should restore for guest evidence");
    assert_eq!(restored.vcpu_count(), 3);
    assert_eq!(restored.vcpu_mpidrs(), [0, 1, 2]);
    let mut restored_vmgenid = [0; ARM64_BOOT_VMGENID_SIZE];
    let mut restored_vmclock_bytes = [0; VMCLOCK_ABI_SIZE];
    {
        let memory = restored
            .guest_memory()
            .expect("clean restored memory should remain mapped");
        memory
            .read_slice(
                &mut restored_vmgenid,
                second_capture.time().vmgenid().range().start(),
            )
            .expect("clean restored VMGenID should read");
        memory
            .read_slice(
                &mut restored_vmclock_bytes,
                second_capture.time().vmclock().range().start(),
            )
            .expect("clean restored VMClock should read");
    }
    assert!(restored_vmgenid.iter().any(|byte| *byte != 0));
    assert_ne!(restored_vmgenid, source_vmgenid);
    assert_ne!(restored_vmgenid, first_clone_vmgenid);
    assert_ne!(restored_vmgenid, second_clone_vmgenid);
    assert_ne!(restored_vmgenid, recaptured_clone_vmgenid);
    let restored_vmclock =
        VmClockAbi::from_bytes(restored_vmclock_bytes).expect("clean VMClock should remain valid");
    assert_vmclock_transition(source_vmclock, restored_vmclock);
    let restored_flags_host = {
        let memory = restored
            .guest_memory()
            .expect("restored memory should remain mapped");
        let region = memory
            .regions()
            .iter()
            .find(|region| region.range().contains(flags))
            .expect("restored flags should belong to mapped DRAM");
        let offset = flags
            .raw_value()
            .checked_sub(region.range().start().raw_value())
            .and_then(|offset| usize::try_from(offset).ok())
            .expect("restored flag host offset should fit");
        region.host_address().as_ptr().cast::<u8>() as usize + offset
    };
    let read_restored_u32 = |offset: usize| {
        // SAFETY: each aligned address remains inside the restored mapping for
        // the platform lifetime; volatile reads observe guest progress.
        unsafe { std::ptr::read_volatile((restored_flags_host + offset) as *const u32) }
    };
    let read_restored_u64 = |offset: usize| {
        // SAFETY: the same owned mapping and alignment argument as above
        // applies to each eight-byte field.
        unsafe { std::ptr::read_volatile((restored_flags_host + offset) as *const u64) }
    };
    assert_eq!(read_restored_u32(PRIMARY_AFTER_RESTORE), 0);
    assert_eq!(read_restored_u32(POST_SUSPEND), 0);
    assert_eq!(read_restored_u32(CPU_TWO_PROGRESS), 0);
    assert_eq!(read_restored_u32(IDENTITY_ACK_COUNT), 0);
    assert_eq!(read_restored_u32(VMGENID_ACK), 0);
    assert_eq!(read_restored_u32(VMCLOCK_ACK), 0);
    assert!(
        read_restored_u32(SOURCE_RTC_SECONDS).wrapping_sub(SOURCE_RTC_SENTINEL) <= 5,
        "the original immutable artifact should retain source guest evidence"
    );
    assert_eq!(read_restored_u64(GUEST_VMGENID_WORD), 0);
    assert_eq!(read_restored_u32(GUEST_VMCLOCK_SEQUENCE), 0);
    assert_eq!(read_restored_u64(GUEST_VMCLOCK_GENERATION), 0);
    assert_eq!(read_restored_u32(GUEST_RTC_SECONDS), 0);
    assert_eq!(read_restored_u64(GUEST_PVTIME_IPA), 0);
    assert_eq!(read_restored_u64(GUEST_PVTIME_STOLEN_NS), 0);

    let rtc_before_resume = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("destination wall clock should follow the Unix epoch")
        .as_secs() as u32;
    restored
        .resume()
        .expect("paused destination should resume once");
    let watchdog_done = Arc::new(AtomicBool::new(false));
    let watchdog_done_for_thread = Arc::clone(&watchdog_done);
    let watchdog_control = restored.control();
    let watchdog = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !watchdog_done_for_thread.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        if !watchdog_done_for_thread.load(Ordering::Acquire) {
            let _ = watchdog_control.request_stop();
        }
    });

    let mut suspend_completed = false;
    let mut cpu_on_two_completed = false;
    let mut cpu_one_offline = false;
    let mut cpu_two_offline = false;
    let mut final_checkpoint = false;
    let mut identity_acknowledgements = Vec::new();
    let mut pvtime_query_completed = false;
    let mut direct_vtimer_exits = 0;
    for _ in 0..64 {
        let step = restored
            .run_step(|entry| {
                entry == secondary_one_entry.raw_value() || entry == secondary_two_entry.raw_value()
            })
            .expect("restored platform step should succeed");
        match step {
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_SUSPEND_64,
                return_value: 0,
                ..
            } => suspend_completed = true,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_ON_64,
                return_value: 0,
                ..
            } => cpu_on_two_completed = true,
            HvfVcpuRunStepOutcome::CpuOff { index: 1, .. } => cpu_one_offline = true,
            HvfVcpuRunStepOutcome::CpuOff { index: 2, .. } => cpu_two_offline = true,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_VERSION,
                return_value: 0x0001_0000,
                ..
            } => final_checkpoint = true,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: IDENTITY_ACK_FUNCTION,
                return_value: 0xffff_ffff,
                ..
            } => {
                let count = read_restored_u32(IDENTITY_ACK_COUNT);
                let acknowledged = match count {
                    1 => read_restored_u32(VMGENID_ACK),
                    2 => read_restored_u32(VMCLOCK_ACK),
                    other => panic!("unexpected identity acknowledgement count {other}"),
                };
                identity_acknowledgements.push(acknowledged);
                assert_eq!(
                    read_restored_u32(PRIMARY_AFTER_RESTORE),
                    0,
                    "ordinary progress must wait for both identity notifications"
                );
            }
            HvfVcpuRunStepOutcome::Hvc {
                function_id: ARM_SMCCC_PV_TIME_ST_64,
                return_value,
                ..
            } => {
                assert_eq!(
                    return_value,
                    second_capture.time().pvtime_vcpus()[0]
                        .record_ipa()
                        .raw_value()
                );
                assert_eq!(
                    identity_acknowledgements,
                    [vmgenid_intid, vmclock_intid],
                    "the guest must acknowledge VMGenID before VMClock"
                );
                assert_eq!(read_restored_u32(PRIMARY_AFTER_RESTORE), 0);
                pvtime_query_completed = true;
            }
            HvfVcpuRunStepOutcome::VtimerActivated => {
                direct_vtimer_exits += 1;
                restored
                    .set_last_step_ppi_pending(virtual_timer_intid)
                    .expect("direct virtual-timer exit should inject its restored PPI");
            }
            _ => {}
        }
        if suspend_completed
            && cpu_on_two_completed
            && cpu_one_offline
            && cpu_two_offline
            && final_checkpoint
            && pvtime_query_completed
        {
            break;
        }
    }
    watchdog_done.store(true, Ordering::Release);
    watchdog.join().expect("native-v2 watchdog should join");
    assert!(suspend_completed, "restored CPU_SUSPEND should complete");
    assert!(
        cpu_on_two_completed,
        "restored primary should start the initially offline vCPU"
    );
    assert!(cpu_one_offline, "woken secondary one should power off");
    assert!(cpu_two_offline, "continued secondary two should power off");
    assert!(
        final_checkpoint,
        "restored primary should reach its checkpoint"
    );
    assert_eq!(
        identity_acknowledgements,
        [vmgenid_intid, vmclock_intid],
        "signed guest IRQ acknowledgements must preserve restore notification order"
    );
    assert!(
        pvtime_query_completed,
        "restored primary should query its PVTime record before ordinary progress"
    );
    assert_eq!(read_restored_u32(PRIMARY_AFTER_RESTORE), 1);
    assert_eq!(read_restored_u32(POST_SUSPEND), 1);
    assert_eq!(read_restored_u64(SUSPEND_RESULT), 0);
    assert_eq!(read_restored_u64(SUSPEND_SENTINEL), SENTINEL);
    assert_eq!(read_restored_u64(CPU_ON_TWO_RESULT), 0);
    assert_eq!(read_restored_u32(CPU_TWO_PROGRESS), 1);
    assert_eq!(read_restored_u32(FINAL_CHECKPOINT), 1);
    assert_eq!(
        read_restored_u64(GUEST_VMGENID_WORD),
        u64::from_le_bytes(
            restored_vmgenid[..8]
                .try_into()
                .expect("VMGenID prefix should be eight bytes")
        ),
        "restored guest must observe the fresh destination VMGenID"
    );
    assert_eq!(
        read_restored_u32(GUEST_VMCLOCK_SEQUENCE),
        restored_vmclock.sequence(),
        "restored guest must observe the completed VMClock sequence"
    );
    assert_eq!(
        read_restored_u64(GUEST_VMCLOCK_GENERATION),
        restored_vmclock.generation_counter(),
        "restored guest must observe the completed VMClock generation"
    );
    let rtc_after_progress = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("destination wall clock should follow the Unix epoch")
        .as_secs() as u32;
    let guest_rtc_seconds = read_restored_u32(GUEST_RTC_SECONDS);
    assert!(
        (rtc_before_resume..=rtc_after_progress).contains(&guest_rtc_seconds),
        "restored guest PL031 value must come from destination SystemTime"
    );
    assert!(
        guest_rtc_seconds.wrapping_sub(SOURCE_RTC_SENTINEL) > 5,
        "restored PL031 must not retain the source guest's mutable RTC load"
    );
    let primary_pvtime = second_capture.time().pvtime_vcpus()[0];
    assert_eq!(
        read_restored_u64(GUEST_PVTIME_IPA),
        primary_pvtime.record_ipa().raw_value(),
        "restored guest must discover its topology-ordered PVTime record"
    );
    assert!(
        read_restored_u64(GUEST_PVTIME_STOLEN_NS) >= primary_pvtime.stolen_time_ns(),
        "restored guest PVTime must continue from the captured accumulator"
    );
    assert!(
        direct_vtimer_exits <= 1,
        "the retained suspended timer should publish its PPI without duplicate direct exits"
    );

    restored
        .control()
        .request_pause()
        .expect("idle continued topology should accept a final pause")
        .wait()
        .expect("final pause should complete");
    let final_memory_artifact = TempFile::new_len("native-v2-platform-final-memory", 0)
        .expect("final memory artifact should create");
    let mut final_writer = OpenOptions::new()
        .read(true)
        .write(true)
        .open(final_memory_artifact.path())
        .expect("final memory artifact should open");
    let final_capture = restored
        .capture_snapshot_v2_platform(&mut final_writer)
        .expect("continued platform should remain capture-ready");
    drop(final_writer);
    assert!(matches!(
        final_capture.topology().members()[0].disposition(),
        HvfArm64StableVcpuDisposition::Runnable
    ));
    assert!(matches!(
        final_capture.topology().members()[1].disposition(),
        HvfArm64StableVcpuDisposition::Offline
    ));
    assert!(matches!(
        final_capture.topology().members()[2].disposition(),
        HvfArm64StableVcpuDisposition::Offline
    ));
    let final_encoded = encode_hvf_snapshot_v2_platform_state(&final_capture)
        .expect("continued platform should encode");
    let final_structural =
        decode_snapshot_v2_state(&final_encoded).expect("continued container should decode");
    let final_decoded = decode_hvf_snapshot_v2_platform_state(&final_structural)
        .expect("continued platform should cross-validate");
    assert_eq!(final_decoded, final_capture);
    restored
        .shutdown()
        .expect("restored native-v2 platform should shut down cleanly");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn psci_cpu_suspend_retains_context_until_two_virtual_timer_wakeups() {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use bangbang_hvf::{
        HvfArm64BootRunLoopOutcome, HvfArm64BootSessionConfig, HvfVcpuRunStepOutcome,
        OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    const SECONDARY_OFFSET: u64 = 0x1000;
    const FLAGS_OFFSET: u64 = 0x4000;
    const FLAGS_SIZE: usize = 0x48;
    const CPU_ON_RESULT: usize = 0x00;
    const AFFINITY_RESULT: usize = 0x08;
    const PRE_SUSPEND_1: usize = 0x10;
    const POST_SUSPEND_1: usize = 0x14;
    const SUSPEND_RESULT_1: usize = 0x18;
    const SENTINEL_1: usize = 0x20;
    const PRE_SUSPEND_2: usize = 0x28;
    const POST_SUSPEND_2: usize = 0x2c;
    const SUSPEND_RESULT_2: usize = 0x30;
    const SENTINEL_2: usize = 0x38;
    const PEER_OBSERVATION: usize = 0x40;
    const PSCI_CPU_SUSPEND_64: u64 = 0xc400_0001;
    const PSCI_VERSION: u64 = 0x8400_0000;
    const SENTINEL: u64 = 0x5a5a;

    // CPU0 starts CPU1, waits for CPU1's pre-suspend publication, observes
    // AFFINITY_INFO, and emits PSCI_VERSION as an event-driven host checkpoint.
    let primary_code = [
        0x1002_0013, // adr x19, flags (+0x4000)
        0xd280_0060, // mov x0, #3
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (CPU_ON64)
        0xd280_0021, // mov x1, #1
        0x1000_7f82, // adr x2, secondary (+0x1000)
        0x1001_ff63, // adr x3, flags (+0x4000)
        0xd400_0002, // hvc #0
        0xf900_0260, // str x0, [x19]
        0xb940_1264, // ldr w4, [x19, #0x10]
        0x34ff_ffe4, // cbz w4, previous instruction
        0xd280_0080, // mov x0, #4
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (AFFINITY_INFO64)
        0xd280_0021, // mov x1, #1
        0xd280_0002, // mov x2, #0
        0xd400_0002, // hvc #0
        0xf900_0660, // str x0, [x19, #8]
        0x5280_0024, // mov w4, #1
        0xb900_4264, // str w4, [x19, #0x40]
        0xd280_0000, // mov x0, #0
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (PSCI_VERSION)
        0xd400_0002, // hvc #0
        0x1400_0000, // b .
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect::<Vec<_>>();

    // CPU1 uses one counter-frequency interval per retained wait, preserves
    // x20 across both calls, and terminates the guest only after both returns.
    let secondary_code = [
        0xaa00_03f3, // mov x19, x0
        0xd28b_4b54, // mov x20, #0x5a5a
        0xd53b_e044, // mrs x4, CNTVCT_EL0
        0xd53b_e005, // mrs x5, CNTFRQ_EL0
        0x8b05_0084, // add x4, x4, x5
        0xd51b_e344, // msr CNTV_CVAL_EL0, x4
        0xd280_0024, // mov x4, #1
        0xd51b_e324, // msr CNTV_CTL_EL0, x4
        0xd503_3fdf, // isb
        0x5280_0026, // mov w6, #1
        0xb900_1266, // str w6, [x19, #0x10]
        0xd280_0020, // mov x0, #1
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (CPU_SUSPEND64)
        0xd295_5541, // mov x1, #0xaaaa (ignored)
        0xd282_4682, // mov x2, #0x1234 (ignored)
        0xd297_dde3, // mov x3, #0xbeef (ignored)
        0xd400_0002, // hvc #0
        0xf900_0e60, // str x0, [x19, #0x18]
        0xf900_1274, // str x20, [x19, #0x20]
        0xb900_1666, // str w6, [x19, #0x14]
        0xd53b_e044, // mrs x4, CNTVCT_EL0
        0xd53b_e005, // mrs x5, CNTFRQ_EL0
        0x8b05_0084, // add x4, x4, x5
        0xd51b_e344, // msr CNTV_CVAL_EL0, x4
        0xd280_0024, // mov x4, #1
        0xd51b_e324, // msr CNTV_CTL_EL0, x4
        0xd503_3fdf, // isb
        0xb900_2a66, // str w6, [x19, #0x28]
        0xd280_0020, // mov x0, #1
        0xf2b8_8000, // movk x0, #0xc400, lsl #16 (CPU_SUSPEND64)
        0xd297_7761, // mov x1, #0xbbbb (ignored)
        0xd28a_cf02, // mov x2, #0x5678 (ignored)
        0xd299_5fc3, // mov x3, #0xcafe (ignored)
        0xd400_0002, // hvc #0
        0xf900_1a60, // str x0, [x19, #0x30]
        0xf900_1e74, // str x20, [x19, #0x38]
        0xb900_2e66, // str w6, [x19, #0x2c]
        0xd280_0100, // mov x0, #8
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (SYSTEM_OFF)
        0xd400_0002, // hvc #0
        0x1400_0000, // b .
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect::<Vec<_>>();

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("psci-cpu-suspend-kernel", &image).expect("temp kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 16)))
        .expect("two-vCPU machine should configure");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("two-vCPU CPU_SUSPEND session should prepare");
    let primary_entry = GuestAddress::new(
        session
            .capture_arm64_general_register_state()
            .expect("primary entry registers should capture")
            .pc(),
    );
    let secondary_entry = primary_entry
        .checked_add(SECONDARY_OFFSET)
        .expect("secondary entry should fit");
    let flags = primary_entry
        .checked_add(FLAGS_OFFSET)
        .expect("shared flags should fit");
    {
        let memory = session
            .guest_memory_mut()
            .expect("guest memory should be mutable before execution");
        memory
            .write_slice(&primary_code, primary_entry)
            .expect("primary guest code should fit");
        memory
            .write_slice(&secondary_code, secondary_entry)
            .expect("secondary guest code should fit");
        memory
            .write_slice(&[0; FLAGS_SIZE], flags)
            .expect("shared guest flags should fit");
    }
    let flags_host = {
        let memory = session
            .guest_memory()
            .expect("mapped guest memory should remain available");
        let region = memory
            .regions()
            .iter()
            .find(|region| region.range().contains(flags))
            .expect("shared flags should belong to mapped DRAM");
        let offset = flags
            .raw_value()
            .checked_sub(region.range().start().raw_value())
            .and_then(|offset| usize::try_from(offset).ok())
            .expect("shared flag host offset should fit");
        region.host_address().as_ptr().cast::<u8>() as usize + offset
    };
    let read_u32 = |offset: usize| {
        // SAFETY: each aligned address remains inside the mapped shared flag
        // area for the session lifetime; volatile reads observe guest stores.
        unsafe { std::ptr::read_volatile((flags_host + offset) as *const u32) }
    };
    let read_u64 = |offset: usize| {
        // SAFETY: each aligned address remains inside the mapped shared flag
        // area for the session lifetime; volatile reads observe guest stores.
        unsafe { std::ptr::read_volatile((flags_host + offset) as *const u64) }
    };

    let control = session.run_loop_control();
    let stop_token = control.stop_token();
    let watchdog_done = Arc::new(AtomicBool::new(false));
    let watchdog_done_for_thread = Arc::clone(&watchdog_done);
    let watchdog = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !watchdog_done_for_thread.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        if !watchdog_done_for_thread.load(Ordering::Acquire) {
            let _ = control.request_stop();
        }
    });

    let one_step = NonZeroUsize::new(1).expect("one is nonzero");
    let mut observed = Vec::new();
    let mut peer_checkpoint_seen = false;
    let mut suspend_entries = 0;
    let mut suspend_completions = 0;
    let mut terminal = None;
    for _ in 0..16 {
        let outcome = session
            .run_loop_with_observer(&stop_token, one_step, |step| observed.push(*step))
            .expect("bounded CPU_SUSPEND run-loop step should succeed");
        let step = *observed
            .last()
            .expect("each non-stopped run-loop call should observe one step");
        match step {
            HvfVcpuRunStepOutcome::CpuSuspend {
                function_id: PSCI_CPU_SUSPEND_64,
                ..
            } => {
                suspend_entries += 1;
                if suspend_entries == 1 {
                    assert_eq!(read_u32(PRE_SUSPEND_1), 1);
                    assert_eq!(read_u32(POST_SUSPEND_1), 0);
                } else if suspend_entries == 2 {
                    assert_eq!(read_u32(POST_SUSPEND_1), 1);
                    assert_eq!(read_u64(SUSPEND_RESULT_1), 0);
                    assert_eq!(read_u64(SENTINEL_1), SENTINEL);
                    assert_eq!(read_u32(PRE_SUSPEND_2), 1);
                    assert_eq!(read_u32(POST_SUSPEND_2), 0);
                }
            }
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_CPU_SUSPEND_64,
                return_value: 0,
                ..
            } => suspend_completions += 1,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: PSCI_VERSION,
                return_value: 0x0001_0000,
                ..
            } => {
                peer_checkpoint_seen = true;
                assert_eq!(read_u64(CPU_ON_RESULT), 0);
                assert_eq!(read_u64(AFFINITY_RESULT), 0);
                assert_eq!(read_u32(PEER_OBSERVATION), 1);
                assert_eq!(read_u32(POST_SUSPEND_1), 0);
            }
            _ => {}
        }
        if matches!(outcome, HvfArm64BootRunLoopOutcome::GuestShutdown { .. }) {
            terminal = Some(outcome);
            break;
        }
        assert!(matches!(
            outcome,
            HvfArm64BootRunLoopOutcome::StepLimitReached { steps: 1 }
        ));
    }

    watchdog_done.store(true, Ordering::Release);
    watchdog.join().expect("CPU_SUSPEND watchdog should join");
    assert!(
        peer_checkpoint_seen,
        "CPU0 should publish its ON-affinity checkpoint"
    );
    assert_eq!(suspend_entries, 2);
    assert_eq!(suspend_completions, 2);
    assert!(matches!(
        terminal,
        Some(HvfArm64BootRunLoopOutcome::GuestShutdown { .. })
    ));
    assert_eq!(read_u32(POST_SUSPEND_2), 1);
    assert_eq!(read_u64(SUSPEND_RESULT_2), 0);
    assert_eq!(read_u64(SENTINEL_2), SENTINEL);
    session
        .shutdown()
        .expect("CPU_SUSPEND session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn psci_1_0_and_smccc_1_1_discovery_match_the_advertised_guest_contract() {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use bangbang_hvf::{
        HvfArm64BootRunLoopOutcome, HvfArm64BootSessionConfig, HvfVcpuRunStepOutcome,
        OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    const RESULTS_OFFSET: u64 = 0x1000;
    const QUERIES_OFFSET: u64 = 0x2000;
    const NOT_SUPPORTED: u64 = 0x0000_0000_ffff_ffff;
    const FEATURE_QUERIES: [(u32, u64); 36] = [
        (0x8400_0000, 0),             // PSCI_VERSION
        (0x8400_0001, 0),             // CPU_SUSPEND32
        (0xc400_0001, 0),             // CPU_SUSPEND64
        (0x8400_0002, 0),             // CPU_OFF
        (0x8400_0003, 0),             // CPU_ON32
        (0xc400_0003, 0),             // CPU_ON64
        (0x8400_0004, 0),             // AFFINITY_INFO32
        (0xc400_0004, 0),             // AFFINITY_INFO64
        (0x8400_0006, 0),             // MIGRATE_INFO_TYPE
        (0x8400_0008, 0),             // SYSTEM_OFF
        (0x8400_0009, 0),             // SYSTEM_RESET
        (0x8400_000a, 0),             // PSCI_FEATURES
        (0x8000_0000, 0),             // SMCCC_VERSION
        (0x8000_0001, NOT_SUPPORTED), // SMCCC_ARCH_FEATURES is not a PSCI query
        (0x8400_0005, NOT_SUPPORTED), // MIGRATE32
        (0xc400_0005, NOT_SUPPORTED), // MIGRATE64
        (0x8400_0007, NOT_SUPPORTED), // MIGRATE_INFO_UP_CPU32
        (0xc400_0007, NOT_SUPPORTED), // MIGRATE_INFO_UP_CPU64
        (0x8400_000b, NOT_SUPPORTED), // CPU_FREEZE
        (0x8400_000c, NOT_SUPPORTED), // CPU_DEFAULT_SUSPEND32
        (0xc400_000c, NOT_SUPPORTED), // CPU_DEFAULT_SUSPEND64
        (0x8400_000d, NOT_SUPPORTED), // NODE_HW_STATE32
        (0xc400_000d, NOT_SUPPORTED), // NODE_HW_STATE64
        (0x8400_000e, NOT_SUPPORTED), // SYSTEM_SUSPEND32
        (0xc400_000e, NOT_SUPPORTED), // SYSTEM_SUSPEND64
        (0x8400_000f, NOT_SUPPORTED), // PSCI_SET_SUSPEND_MODE
        (0x8400_0010, NOT_SUPPORTED), // PSCI_STAT_RESIDENCY32
        (0xc400_0010, NOT_SUPPORTED), // PSCI_STAT_RESIDENCY64
        (0x8400_0011, NOT_SUPPORTED), // PSCI_STAT_COUNT32
        (0xc400_0011, NOT_SUPPORTED), // PSCI_STAT_COUNT64
        (0x8400_0012, NOT_SUPPORTED), // SYSTEM_RESET2_32
        (0xc400_0012, NOT_SUPPORTED), // SYSTEM_RESET2_64
        (0x8400_0013, NOT_SUPPORTED), // MEM_PROTECT
        (0x8400_0014, NOT_SUPPORTED), // MEM_PROTECT_CHECK_RANGE32
        (0xc400_0014, NOT_SUPPORTED), // MEM_PROTECT_CHECK_RANGE64
        (0xdead_beef, NOT_SUPPORTED), // unknown
    ];
    const EXTRA_RESULT_COUNT: usize = 10;
    const RESULT_COUNT: usize = FEATURE_QUERIES.len() + EXTRA_RESULT_COUNT;
    const RESULTS_SIZE: usize = RESULT_COUNT * size_of::<u64>();
    const PSCI_VERSION: u64 = 0x8400_0000;
    const PSCI_FEATURES: u64 = 0x8400_000a;
    const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
    const ARM_SMCCC_VERSION: u64 = 0x8000_0000;
    const ARM_SMCCC_ARCH_FEATURES: u64 = 0x8000_0001;
    const ARM_SMCCC_PV_TIME_FEATURES_64: u64 = 0xc500_0020;
    const ARM_SMCCC_PV_TIME_ST_64: u64 = 0xc500_0021;

    // Loop over the host-supplied PSCI_FEATURES table, then query PSCI and
    // SMCCC versions plus the mandatory minimum SMCCC_ARCH_FEATURES boundary.
    let guest_code = [
        0x1000_8013, // adr x19, results (+0x1000)
        0x1000_fff4, // adr x20, queries (+0x2000)
        0x5280_0495, // mov w21, #36
        0xb840_4681, // ldr w1, [x20], #4
        0xd280_0140, // mov x0, #0xa
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (PSCI_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0x7100_06b5, // subs w21, w21, #1
        0x54ff_ff41, // b.ne feature loop
        0xd280_0000, // mov x0, #0
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (PSCI_VERSION)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0000, // mov x0, #0
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_VERSION)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0001, // mov x1, #0
        0xf2b0_0001, // movk x1, #0x8000, lsl #16 (SMCCC_VERSION query)
        0xd280_0020, // mov x0, #1
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_ARCH_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0021, // mov x1, #1
        0xf2b0_0001, // movk x1, #0x8000, lsl #16 (self query)
        0xd280_0020, // mov x0, #1
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_ARCH_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd290_0001, // mov x1, #0x8000
        0xf2b0_0001, // movk x1, #0x8000, lsl #16 (WORKAROUND_1 query)
        0xd280_0020, // mov x0, #1
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_ARCH_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0401, // mov x1, #0x20
        0xf2b8_a001, // movk x1, #0xc500, lsl #16 (PV_TIME_FEATURES query)
        0xd280_0020, // mov x0, #1
        0xf2b0_0000, // movk x0, #0x8000, lsl #16 (SMCCC_ARCH_FEATURES)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0421, // mov x1, #0x21
        0xf2b8_a001, // movk x1, #0xc500, lsl #16 (PV_TIME_ST query)
        0xd280_0400, // mov x0, #0x20
        0xf2b8_a000, // movk x0, #0xc500, lsl #16 (PV_TIME_FEATURES64)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0420, // mov x0, #0x21
        0xf2b8_a000, // movk x0, #0xc500, lsl #16 (PV_TIME_ST64)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0400, // mov x0, #0x20
        0xf2b0_a000, // movk x0, #0x8500, lsl #16 (PV_TIME_FEATURES32)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0420, // mov x0, #0x21
        0xf2b0_a000, // movk x0, #0x8500, lsl #16 (PV_TIME_ST32)
        0xd400_0002, // hvc #0
        0xf800_8660, // str x0, [x19], #8
        0xd280_0100, // mov x0, #8
        0xf2b0_8000, // movk x0, #0x8400, lsl #16 (SYSTEM_OFF)
        0xd400_0002, // hvc #0
        0x1400_0000, // b .
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect::<Vec<_>>();
    let query_bytes = FEATURE_QUERIES
        .into_iter()
        .flat_map(|(function_id, _)| function_id.to_le_bytes())
        .collect::<Vec<_>>();

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("psci-discovery-kernel", &image)
        .expect("temporary PSCI discovery kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(1, 16)))
        .expect("one-vCPU discovery machine should configure");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("PSCI discovery session should prepare");
    let pvtime_record_ipa = session
        .runtime_resources()
        .pvtime_state
        .layout()
        .expect("prepared session should retain its advertised PVTime layout")
        .records()[0]
        .start()
        .raw_value();
    assert!(session.runtime_resources().pvtime_state.advertised());
    let entry = GuestAddress::new(
        session
            .capture_arm64_general_register_state()
            .expect("discovery entry registers should capture")
            .pc(),
    );
    let results = entry
        .checked_add(RESULTS_OFFSET)
        .expect("discovery results should fit");
    let queries = entry
        .checked_add(QUERIES_OFFSET)
        .expect("discovery queries should fit");
    {
        let memory = session
            .guest_memory_mut()
            .expect("discovery guest memory should be mutable before execution");
        memory
            .write_slice(&guest_code, entry)
            .expect("discovery guest code should fit");
        memory
            .write_slice(&query_bytes, queries)
            .expect("discovery query table should fit");
        memory
            .write_slice(&[0; RESULTS_SIZE], results)
            .expect("discovery result table should fit");
    }

    let control = session.run_loop_control();
    let stop_token = control.stop_token();
    let watchdog_done = Arc::new(AtomicBool::new(false));
    let watchdog_done_for_thread = Arc::clone(&watchdog_done);
    let watchdog = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !watchdog_done_for_thread.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::yield_now();
        }
        if !watchdog_done_for_thread.load(Ordering::Acquire) {
            let _ = control.request_stop();
        }
    });

    let mut observed = Vec::new();
    let outcome = session
        .run_loop_with_observer(
            &stop_token,
            NonZeroUsize::new(64).expect("step limit should be nonzero"),
            |step| observed.push(*step),
        )
        .expect("bounded PSCI discovery guest should run");
    watchdog_done.store(true, Ordering::Release);
    watchdog
        .join()
        .expect("PSCI discovery watchdog should join");
    assert!(matches!(
        outcome,
        HvfArm64BootRunLoopOutcome::GuestShutdown { .. }
    ));
    assert_eq!(
        observed
            .iter()
            .filter(|step| matches!(
                step,
                HvfVcpuRunStepOutcome::Hvc {
                    function_id: PSCI_FEATURES,
                    ..
                }
            ))
            .count(),
        FEATURE_QUERIES.len()
    );
    assert!(observed.iter().any(|step| matches!(
        step,
        HvfVcpuRunStepOutcome::Hvc {
            function_id: PSCI_VERSION,
            return_value: 0x0001_0000,
            ..
        }
    )));
    assert!(observed.iter().any(|step| matches!(
        step,
        HvfVcpuRunStepOutcome::Hvc {
            function_id: ARM_SMCCC_VERSION,
            return_value: 0x0001_0001,
            ..
        }
    )));
    assert_eq!(
        observed
            .iter()
            .filter(|step| matches!(
                step,
                HvfVcpuRunStepOutcome::Hvc {
                    function_id: ARM_SMCCC_ARCH_FEATURES,
                    ..
                }
            ))
            .count(),
        4
    );
    assert!(observed.iter().any(|step| matches!(
        step,
        HvfVcpuRunStepOutcome::Hvc {
            function_id: ARM_SMCCC_PV_TIME_FEATURES_64,
            return_value: 0,
            ..
        }
    )));
    assert!(observed.iter().any(|step| {
        matches!(
            step,
            HvfVcpuRunStepOutcome::Hvc {
                function_id: ARM_SMCCC_PV_TIME_ST_64,
                return_value,
                ..
            } if *return_value == pvtime_record_ipa
        )
    }));
    assert!(matches!(
        observed.last(),
        Some(HvfVcpuRunStepOutcome::GuestShutdown {
            function_id: PSCI_SYSTEM_OFF,
            ..
        })
    ));

    let mut result_bytes = [0; RESULTS_SIZE];
    session
        .guest_memory()
        .expect("discovery guest memory should remain mapped")
        .read_slice(&mut result_bytes, results)
        .expect("discovery results should read after terminal exit");
    let actual = result_bytes
        .chunks_exact(size_of::<u64>())
        .map(|bytes| u64::from_le_bytes(bytes.try_into().expect("result chunk should be u64")))
        .collect::<Vec<_>>();
    let mut expected = FEATURE_QUERIES
        .iter()
        .map(|(_, result)| *result)
        .collect::<Vec<_>>();
    expected.extend([
        0x0001_0000,
        0x0001_0001,
        0,
        0,
        NOT_SUPPORTED,
        0,
        0,
        pvtime_record_ipa,
        u64::MAX,
        u64::MAX,
    ]);
    assert_eq!(actual, expected);

    session
        .shutdown()
        .expect("PSCI discovery session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn captures_native_v1_composite_and_keeps_source_session_usable() {
    use std::io::Cursor;
    use std::time::Instant;

    use bangbang_hvf::{
        HvfArm64BootSerialDeviceConfig, HvfArm64BootSessionConfig, HvfSnapshotV1Bundle,
        HvfVcpuRunStepOutcome, OwnedHvfArm64BootSession, PreparedHvfSnapshotV1Load,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::{BlockMmioLayout, DriveConfigInput};
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::serial::{SharedSerialOutput, SharedSerialOutputBuffer};
    use bangbang_runtime::snapshot_artifact::{
        SnapshotCommitDurability, load_snapshot_artifacts, publish_snapshot_artifacts_with,
    };
    use bangbang_runtime::snapshot_commit::SnapshotCommitKind;
    use bangbang_runtime::snapshot_device::{
        decode_snapshot_v1_device_state, encode_snapshot_v1_device_state,
    };
    use bangbang_runtime::snapshot_memory::write_snapshot_memory_image;
    use bangbang_runtime::startup::prepare_snapshot_v1_device_profile;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("snapshot-device-kernel", &image).expect("temp kernel should be created");
    let root = TempFile::new_len("snapshot-device-root", 4096)
        .expect("temp root backing should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");
    controller
        .handle_action(VmmAction::PutDrive(
            DriveConfigInput::new("rootfs", "rootfs", root.path(), true).with_is_read_only(true),
        ))
        .expect("read-only root config should be stored");
    controller
        .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(1, 16)))
        .expect("compact snapshot test machine should configure");

    let serial_buffer = SharedSerialOutputBuffer::default();
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x5800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x7000_0000), MmioRegionId::new(2000)),
        bangbang_runtime::rtc::RtcMmioLayout::new(
            GuestAddress::new(0x4000_1000),
            MmioRegionId::new(10),
        ),
    )
    .with_serial_device(HvfArm64BootSerialDeviceConfig::new(
        MmioRegionId::new(20),
        GuestAddress::new(0x4000_2000),
        SharedSerialOutput::from(serial_buffer),
    ));
    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("owned snapshot device session should prepare");

    // Write deterministic guest code at the configured entry. The first exit
    // stores a non-default serial scratch register, the second is a PSCI HVC,
    // and the final HVC remains after both captures to prove source resumption.
    let source_entry = GuestAddress::new(
        session
            .capture_arm64_general_register_state()
            .expect("source entry registers should capture")
            .pc(),
    );
    let guest_code = [
        0xd282_4685, // mov x5, #0x1234
        0xd2a8_0001, // mov x1, #0x40000000
        0xf284_00e1, // movk x1, #0x2007
        0xd280_0b42, // mov x2, #0x5a
        0x3900_0022, // strb w2, [x1]
        0xd2b0_8000, // mov x0, #0x84000000 (PSCI_VERSION)
        0xd400_0002, // hvc #0
        0xd28a_cf06, // mov x6, #0x5678
        0xd2b0_8000, // mov x0, #0x84000000 (PSCI_VERSION)
        0xd400_0002, // hvc #0
    ]
    .into_iter()
    .flat_map(u32::to_le_bytes)
    .collect::<Vec<_>>();
    session
        .guest_memory_mut()
        .expect("source guest memory should be mutable before execution")
        .write_slice(&guest_code, source_entry)
        .expect("source guest code should fit at the configured entry");
    assert!(matches!(
        session.run_once_and_handle_mmio(),
        Ok(HvfVcpuRunStepOutcome::Mmio { .. })
    ));
    assert!(matches!(
        session.run_once_and_handle_mmio(),
        Ok(HvfVcpuRunStepOutcome::Hvc {
            function_id: 0x8400_0000,
            return_value: 0x0001_0000,
            ..
        })
    ));

    let artifact_pair = TempSnapshotArtifacts::new("native-v1-composite")
        .expect("snapshot artifact directory should create");
    let artifact_paths = artifact_pair.paths();
    let guard = session
        .quiesce_limiter_retry_wakeups()
        .expect("snapshot device retry work should quiesce");
    let publication = publish_snapshot_artifacts_with(&artifact_paths, |mut writer| {
        let state = session
            .capture_snapshot_v1_state_at(
                &controller.drive_configs()[0],
                controller.serial_config(),
                &guard,
                Instant::now(),
            )
            .expect("complete inactive native-v1 state should capture");
        let binding = write_snapshot_memory_image(
            session
                .guest_memory()
                .expect("source guest memory should remain mapped"),
            &mut writer,
        )
        .expect("source guest memory should stream while quiesced");
        let bundle = HvfSnapshotV1Bundle::try_new(binding, state)
            .expect("complete state and memory should form one bundle");
        drop(writer);
        Ok::<_, std::convert::Infallible>(bundle.into_commit_record())
    })
    .expect("production publisher should commit complete native-v1 capture");
    // Keep all block, PMEM, network, and entropy retry schedulers quiesced
    // through validation, durability barriers, and the no-clobber commit.
    drop(guard);
    assert_eq!(publication.durability(), SnapshotCommitDurability::Durable);
    assert_eq!(publication.record().kind(), SnapshotCommitKind::Composite);
    artifact_pair
        .assert_committed_without_staging()
        .expect("committed artifact directory should contain no staging entries");

    let artifacts = load_snapshot_artifacts(&artifact_paths)
        .expect("production-published artifact pair should validate and load");
    assert_eq!(artifacts.record(), publication.record());
    let bundle = HvfSnapshotV1Bundle::try_from_commit_record(artifacts.record().clone())
        .expect("published composite commit should decode");
    let loaded_memory = artifacts.memory();
    assert_eq!(
        loaded_memory.total_size(),
        session.runtime_resources().layout.total_size()
    );

    let encoded = encode_snapshot_v1_device_state(bundle.state().device())
        .expect("captured snapshot device state should encode");
    let decoded = decode_snapshot_v1_device_state(&encoded)
        .expect("captured snapshot device state should decode");
    assert_eq!(decoded.serial_state().scratch(), 0x5a);
    let mut source_generation_id = [0; 16];
    loaded_memory
        .read_slice(&mut source_generation_id, decoded.vmgenid().range().start())
        .expect("captured VMGenID bytes should read");

    let prepared = prepare_snapshot_v1_device_profile(&decoded, loaded_memory, Instant::now())
        .expect("decoded inactive device profile should prepare off-side");

    assert!(!prepared.block_handler().is_device_activated());
    assert!(
        prepared.drive_config().path_on_host() == controller.drive_configs()[0].path_on_host(),
        "prepared drive path should match without logging either path"
    );
    assert!(
        prepared.vmgenid_device().range == decoded.vmgenid().range(),
        "prepared VMGenID range should match without logging guest addresses"
    );
    assert!(
        prepared.vmclock_device().range == decoded.vmclock().range(),
        "prepared VMClock range should match without logging guest addresses"
    );
    drop(prepared);

    let first_image_id = bundle.commit_record().memory_binding().image_id();
    let second_image_id = {
        let guard = session
            .quiesce_limiter_retry_wakeups()
            .expect("second snapshot retry work should quiesce");
        let state = session
            .capture_snapshot_v1_state_at(
                &controller.drive_configs()[0],
                controller.serial_config(),
                &guard,
                Instant::now(),
            )
            .expect("second complete native-v1 state should capture");
        let mut memory_image = Cursor::new(Vec::new());
        let binding = write_snapshot_memory_image(
            session
                .guest_memory()
                .expect("source guest memory should remain mapped for retry"),
            &mut memory_image,
        )
        .expect("second memory image should stream");
        HvfSnapshotV1Bundle::try_new(binding, state)
            .expect("second complete bundle should validate")
            .commit_record()
            .memory_binding()
            .image_id()
    };
    assert_ne!(first_image_id, second_image_id);
    assert!(matches!(
        session.run_once_and_handle_mmio(),
        Ok(HvfVcpuRunStepOutcome::Hvc {
            function_id: 0x8400_0000,
            return_value: 0x0001_0000,
            ..
        })
    ));
    session
        .capture_arm64_general_register_state()
        .expect("source owner should remain usable after resumption");
    session
        .shutdown()
        .expect("owned snapshot device session should shut down");

    let prepared = PreparedHvfSnapshotV1Load::from_loaded_artifacts(artifacts, Instant::now())
        .expect("production-published pair should prepare without constructing a VM");
    assert!(prepared.runtime().runtime_resources.boot_origin.is_none());

    let restored = OwnedHvfArm64BootSession::restore_snapshot_v1(prepared, true)
        .expect("fresh tracked destination VM should restore from native-v1 artifacts");
    let (mut restored_session, restored_drive, _serial_output, restored_serial_buffer) =
        restored.into_parts();
    assert!(restored_session.boot_registers().is_none());
    assert!(restored_session.runtime_resources().boot_origin.is_none());
    assert!(
        restored_session.arm64_fdt_cache_hierarchy().is_none(),
        "native-v1 restore must not invent cache presentation absent from the schema"
    );
    assert_eq!(restored_drive, controller.drive_configs()[0]);
    assert_eq!(
        restored_serial_buffer
            .bytes()
            .expect("restored serial buffer should read"),
        Vec::<u8>::new()
    );

    let mut destination_generation_id = [0; 16];
    restored_session
        .guest_memory()
        .expect("restored destination memory should remain mapped")
        .read_slice(
            &mut destination_generation_id,
            decoded.vmgenid().range().start(),
        )
        .expect("restored VMGenID bytes should read");
    assert_ne!(destination_generation_id, source_generation_id);
    assert_ne!(destination_generation_id, [0; 16]);
    assert!(
        restored_session
            .runtime_resources()
            .machine_config
            .track_dirty_pages(),
        "the destination load request must override the source tracking flag"
    );
    let restored_tracker = restored_session
        .guest_memory()
        .expect("tracked restored memory should remain mapped")
        .dirty_tracker()
        .expect("tracked restore should retain one shared dirty epoch");
    let page_size = host_page_size().expect("host page size should remain available");
    let vmgenid_page =
        GuestAddress::new(decoded.vmgenid().range().start().raw_value() & !(page_size - 1));
    assert_eq!(
        restored_tracker
            .dirty_pages()
            .expect("post-baseline VMGenID dirty page should query"),
        vec![vmgenid_page],
        "snapshot memory is the clean baseline and VMGenID is the first host write"
    );
    assert_eq!(restored_session.reset_dirty_epoch_quiesced(), Ok(Some(1)));
    assert!(
        restored_tracker
            .dirty_pages()
            .expect("committed restore epoch should clear")
            .is_empty()
    );

    let restored_state = {
        let guard = restored_session
            .quiesce_limiter_retry_wakeups()
            .expect("restored retry work should quiesce before first run");
        restored_session
            .capture_snapshot_v1_state_at(
                &restored_drive,
                &bangbang_runtime::serial::SerialConfig::default(),
                &guard,
                Instant::now(),
            )
            .expect("restored destination state should recapture before first run")
    };
    assert_eq!(restored_state.vcpu(), bundle.state().vcpu());
    assert_eq!(
        restored_state.interrupts().pending_interrupts,
        bundle.state().interrupts().pending_interrupts
    );
    assert!(
        !restored_state.interrupts().gic_device.is_empty(),
        "HVF should recapture a nonempty opaque GIC state after restore"
    );
    assert!(
        restored_state.interrupts().gic_device.len()
            <= bangbang_hvf::HVF_SNAPSHOT_V1_GIC_DEVICE_STATE_MAX_BYTES,
        "recaptured opaque GIC state should remain within the native-v1 bound"
    );
    assert_eq!(
        restored_state.interrupts().gic_icc,
        bundle.state().interrupts().gic_icc
    );
    assert_normalized_timer_restore_equivalent(
        bundle.state().interrupts().timer,
        restored_state.interrupts().timer,
    );
    assert_eq!(restored_state.device().serial_state().scratch(), 0x5a);

    assert!(matches!(
        restored_session.run_once_and_handle_mmio(),
        Ok(HvfVcpuRunStepOutcome::Hvc {
            function_id: 0x8400_0000,
            return_value: 0x0001_0000,
            ..
        })
    ));
    assert_eq!(
        restored_session
            .capture_arm64_general_register_state()
            .expect("restored destination registers should capture after continuation")
            .general_purpose_register(6),
        Some(0x5678)
    );
    restored_session
        .shutdown()
        .expect("restored destination session should shut down");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn applies_and_verifies_mixed_width_arm64_cpu_template_on_two_hvf_vcpus() {
    use bangbang_hvf::{
        ARM64_LINUX_BOOT_CPSR, HvfArm64BootSessionConfig, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::cpu::{
        CpuConfigArmRegisterModifier, CpuConfigArmRegisterWidth, CpuConfigInput,
        KVM_REG_ARM64_ACTLR_EL1, KVM_REG_ARM64_CORE_ELR_EL1, KVM_REG_ARM64_CORE_FPCR,
        KVM_REG_ARM64_CORE_FPSR, KVM_REG_ARM64_CORE_PC, KVM_REG_ARM64_CORE_PSTATE,
        KVM_REG_ARM64_CORE_SP_EL0, KVM_REG_ARM64_CORE_SP_EL1, KVM_REG_ARM64_CORE_SPSR_EL1,
        KVM_REG_ARM64_ID_AA64DFR0_EL1, KVM_REG_ARM64_ID_AA64DFR1_EL1,
        KVM_REG_ARM64_ID_AA64ISAR0_EL1, KVM_REG_ARM64_ID_AA64ISAR1_EL1,
        KVM_REG_ARM64_ID_AA64MMFR0_EL1, KVM_REG_ARM64_ID_AA64MMFR1_EL1,
        KVM_REG_ARM64_ID_AA64MMFR2_EL1, KVM_REG_ARM64_ID_AA64PFR0_EL1,
        KVM_REG_ARM64_ID_AA64PFR1_EL1, KVM_REG_ARM64_ID_AA64SMFR0_EL1,
        KVM_REG_ARM64_ID_AA64ZFR0_EL1, kvm_reg_arm64_core_q, kvm_reg_arm64_core_x,
    };
    use bangbang_runtime::machine::MachineConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let image = arm64_image().expect("test arm64 image should build");
    let kernel = TempFile::new("cpu-template-kernel", &image)
        .expect("temporary CPU-template kernel should be created");
    let modifier = CpuConfigArmRegisterModifier::new;
    let x0_target = 0x1111_2222_3333_4444_u128;
    let x4_target = 0xffff_eeee_dddd_cccc_u128;
    let x30_target = 0x0123_4567_89ab_cdef_u128;
    let pc_target = 0x2000_u128;
    let pstate_target = 0xa000_0000_u128;
    let sp_el0_target = 0x7777_0000_u128;
    let sp_el1_target = 0x8888_0000_u128;
    let elr_el1_target = 0x9999_0000_u128;
    let spsr_el1_target = u128::from(ARM64_LINUX_BOOT_CPSR);
    let q0_target = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff_u128;
    let q31_target = 0xffee_ddcc_bbaa_9988_7766_5544_3322_1100_u128;
    let fpcr_target = 1_u128 << 22;
    let fpsr_target = 0x11_u128;
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutMachineConfig(MachineConfigInput::new(2, 16)))
        .expect("two-vCPU machine config should store");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should store");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let mut baseline_session = OwnedHvfArm64BootSession::new(&controller, config.clone())
        .expect("disposable two-vCPU baseline session should prepare");
    let baseline_identification = baseline_session
        .capture_arm64_identification_register_state()
        .expect("baseline identification state should capture without logging values");
    let baseline_optional_identification = baseline_session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("baseline optional identification state should capture without logging values");
    let baseline_execution = baseline_session
        .capture_arm64_execution_control_register_state()
        .expect("baseline execution-control state should capture without logging values");
    baseline_session
        .shutdown()
        .expect("disposable baseline session should shut down cleanly");

    controller
        .handle_action(VmmAction::PutCpuConfig(CpuConfigInput::new(
            Vec::new(),
            vec![
                modifier(
                    kvm_reg_arm64_core_x(0).expect("X0 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    x0_target,
                ),
                modifier(
                    kvm_reg_arm64_core_x(4).expect("X4 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    x4_target,
                ),
                modifier(
                    kvm_reg_arm64_core_x(30).expect("X30 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    x30_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_PC,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    pc_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_PSTATE,
                    CpuConfigArmRegisterWidth::U64,
                    0xf000_0000,
                    pstate_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_SP_EL0,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    sp_el0_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_SP_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    sp_el1_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_ELR_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    elr_el1_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_SPSR_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    spsr_el1_target,
                ),
                modifier(
                    kvm_reg_arm64_core_q(0).expect("Q0 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U128,
                    u128::MAX,
                    q0_target,
                ),
                modifier(
                    kvm_reg_arm64_core_q(31).expect("Q31 should have a KVM identity"),
                    CpuConfigArmRegisterWidth::U128,
                    u128::MAX,
                    q31_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_FPCR,
                    CpuConfigArmRegisterWidth::U32,
                    0x00c0_0000,
                    fpcr_target,
                ),
                modifier(
                    KVM_REG_ARM64_CORE_FPSR,
                    CpuConfigArmRegisterWidth::U32,
                    0x1f,
                    fpsr_target,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64PFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0x000f_000f_0000_0000,
                    0,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64ISAR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0xf0ff_0fff_0000_f000,
                    0x1000,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64ISAR1_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0x00ff_f000_00ff_f00f,
                    0x0010_0001,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64MMFR2_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    0x0000_000f_0000_0000,
                    0,
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64PFR1_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64pfr1_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64DFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64dfr0_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64DFR1_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64dfr1_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64MMFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64mmfr0_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64MMFR1_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_identification.id_aa64mmfr1_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64ZFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_optional_identification.id_aa64zfr0_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ID_AA64SMFR0_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    u64::MAX.into(),
                    baseline_optional_identification.id_aa64smfr0_el1().into(),
                ),
                modifier(
                    KVM_REG_ARM64_ACTLR_EL1,
                    CpuConfigArmRegisterWidth::U64,
                    2,
                    2,
                ),
            ],
            Vec::new(),
        )))
        .expect("mixed-width CPU template should store");

    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("mixed-width template should write and read back on both HVF vCPUs");
    let boot_registers = session
        .boot_registers()
        .expect("ordinary CPU-template session should retain boot registers");
    let general = session
        .capture_arm64_general_register_state()
        .expect("mixed-width CPU-template general state should capture");
    assert!(
        general.general_purpose_register(0) == Some(boot_registers.fdt_address.raw_value()),
        "Linux boot setup must override the template's X0 value"
    );
    assert!(
        general.general_purpose_register(4) == u64::try_from(x4_target).ok(),
        "X4 must retain the exact mixed-width CPU-template target"
    );
    assert!(
        general.general_purpose_register(30) == u64::try_from(x30_target).ok(),
        "X30 must retain the exact mixed-width CPU-template target"
    );
    assert!(
        general.pc() == boot_registers.kernel_entry.raw_value(),
        "Linux boot setup must override the template's PC value"
    );
    assert!(
        general.cpsr() == ARM64_LINUX_BOOT_CPSR,
        "Linux boot setup must override the template's PSTATE value"
    );
    let core_system = session
        .capture_arm64_core_system_register_state()
        .expect("mixed-width CPU-template core system state should capture");
    assert!(
        core_system.sp_el0() == u64::try_from(sp_el0_target).expect("target should fit U64"),
        "SP_EL0 must retain the exact CPU-template target"
    );
    assert!(
        core_system.sp_el1() == u64::try_from(sp_el1_target).expect("target should fit U64"),
        "SP_EL1 must retain the exact CPU-template target"
    );
    assert!(
        core_system.elr_el1() == u64::try_from(elr_el1_target).expect("target should fit U64"),
        "ELR_EL1 must retain the exact CPU-template target"
    );
    assert!(
        core_system.spsr_el1() == u64::try_from(spsr_el1_target).expect("target should fit U64"),
        "SPSR_EL1 must retain the exact CPU-template target"
    );
    let simd_fp = session
        .capture_arm64_simd_fp_state()
        .expect("mixed-width CPU-template SIMD/FP state should capture");
    assert!(
        simd_fp.q_register(0) == Some(q0_target.to_le_bytes()),
        "Q0 must retain the exact little-endian CPU-template target"
    );
    assert!(
        simd_fp.q_register(31) == Some(q31_target.to_le_bytes()),
        "Q31 must retain the exact little-endian CPU-template target"
    );
    assert!(
        simd_fp.fpcr() == u64::try_from(fpcr_target).expect("target should fit U64"),
        "FPCR must retain the zero-extended U32 CPU-template target"
    );
    assert!(
        simd_fp.fpsr() == u64::try_from(fpsr_target).expect("target should fit U64"),
        "FPSR must retain the zero-extended U32 CPU-template target"
    );
    let identification = session
        .capture_arm64_identification_register_state()
        .expect("complete CPU-template identification state should capture");
    let identification_again = session
        .capture_arm64_identification_register_state()
        .expect("complete CPU-template identification state should recapture");
    assert!(
        identification == identification_again,
        "baseline-preserving ID targets must remain stable after exact transaction readback"
    );
    assert!(
        identification.id_aa64pfr1_el1() == baseline_identification.id_aa64pfr1_el1()
            && identification.id_aa64dfr0_el1() == baseline_identification.id_aa64dfr0_el1()
            && identification.id_aa64dfr1_el1() == baseline_identification.id_aa64dfr1_el1()
            && identification.id_aa64mmfr0_el1() == baseline_identification.id_aa64mmfr0_el1()
            && identification.id_aa64mmfr1_el1() == baseline_identification.id_aa64mmfr1_el1(),
        "all five new baseline-tier ID targets must match the disposable host baseline"
    );
    let optional_identification = session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("optional CPU-template identification state should capture");
    let optional_identification_again = session
        .capture_arm64_sve_sme_identification_register_state()
        .expect("optional CPU-template identification state should recapture");
    assert!(
        optional_identification == optional_identification_again,
        "baseline-preserving ZFR0/SMFR0 targets must remain stable after exact readback"
    );
    assert!(
        optional_identification == baseline_optional_identification,
        "both optional ID targets must match the disposable host baseline"
    );
    let execution = session
        .capture_arm64_execution_control_register_state()
        .expect("CPU-template ACTLR state should capture");
    assert!(
        execution.actlr_el1() == (baseline_execution.actlr_el1() | 2),
        "ACTLR.EnTSO must retain the exact documented CPU-template target"
    );
    session
        .shutdown()
        .expect("CPU-template session should shut down cleanly");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn owned_hvf_arm64_boot_session_cleans_up_after_prepare_error() {
    use bangbang_hvf::{
        HvfArm64BootSessionConfig, HvfArm64BootSessionError, OwnedHvfArm64BootSession,
    };
    use bangbang_runtime::VmmAction;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::boot::BootSourceConfigInput;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::startup::Arm64BootResourceError;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let config = HvfArm64BootSessionConfig::new(
        BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
        PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
        NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
        VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
        test_rtc_mmio_layout(),
    );
    let empty_controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");

    let err = OwnedHvfArm64BootSession::new(&empty_controller, config.clone())
        .expect_err("missing boot source should fail owned HVF session preparation");
    assert!(matches!(
        err,
        HvfArm64BootSessionError::AssembleResources {
            source: Arm64BootResourceError::MissingBootSource
        }
    ));

    let image = arm64_image().expect("test arm64 image should build");
    let kernel =
        TempFile::new("owned-session-retry-kernel", &image).expect("temp kernel should be created");
    let mut controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    controller
        .handle_action(VmmAction::PutBootSource(BootSourceConfigInput::new(
            kernel.path(),
        )))
        .expect("boot source config should be stored");

    let mut session = OwnedHvfArm64BootSession::new(&controller, config)
        .expect("owned HVF arm64 boot session should prepare after failed preparation");
    session
        .shutdown()
        .expect("owned HVF arm64 boot session should shut down after retry");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[test]
fn rejects_boot_session_on_existing_hvf_vm_without_destroying_it() {
    use bangbang_hvf::{HvfArm64BootSessionConfig, HvfArm64BootSessionError, HvfBackend};
    use bangbang_runtime::VmBackend;
    use bangbang_runtime::block::BlockMmioLayout;
    use bangbang_runtime::memory::GuestAddress;
    use bangbang_runtime::mmio::MmioRegionId;
    use bangbang_runtime::network::NetworkMmioLayout;
    use bangbang_runtime::pmem::PmemMmioLayout;
    use bangbang_runtime::vsock::VsockMmioLayout;

    let _test_lock = HVF_LIFECYCLE_TEST_LOCK
        .lock()
        .expect("HVF lifecycle test lock should not be poisoned");
    let controller = bangbang_runtime::VmmController::new("test", "0.1.0", "bangbang");
    let mut backend = HvfBackend::new();
    backend.create_vm().expect("existing VM should be created");

    let err = backend
        .prepare_arm64_boot_session(
            &controller,
            HvfArm64BootSessionConfig::new(
                BlockMmioLayout::new(GuestAddress::new(0x4000_0000), MmioRegionId::new(1)),
                PmemMmioLayout::new(GuestAddress::new(0x4800_0000), MmioRegionId::new(500)),
                NetworkMmioLayout::new(GuestAddress::new(0x5000_0000), MmioRegionId::new(1000)),
                VsockMmioLayout::new(GuestAddress::new(0x6000_0000), MmioRegionId::new(2000)),
                test_rtc_mmio_layout(),
            ),
        )
        .expect_err("existing VM should be rejected");

    assert!(matches!(
        err,
        HvfArm64BootSessionError::BackendAlreadyInitialized
    ));
    let _metadata = backend
        .create_gic()
        .expect("existing VM should remain available after rejected session");
    backend
        .destroy_vm()
        .expect("existing VM should remain owned by caller");
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn host_page_size() -> Result<u64, std::num::TryFromIntError> {
    // SAFETY: `sysconf(_SC_PAGESIZE)` has no pointer arguments and does not
    // require process-local invariants from Rust.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };

    u64::try_from(page_size)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[derive(Clone, Copy, Debug, Default)]
struct ProcessMemoryUsage {
    virtual_size: u64,
    resident_size: u64,
    faults: u64,
    pageins: u64,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl ProcessMemoryUsage {
    const fn saturating_growth_from(self, baseline: Self) -> Self {
        Self {
            virtual_size: self.virtual_size.saturating_sub(baseline.virtual_size),
            resident_size: self.resident_size.saturating_sub(baseline.resident_size),
            faults: self.faults.saturating_sub(baseline.faults),
            pageins: self.pageins.saturating_sub(baseline.pageins),
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn process_memory_usage() -> std::io::Result<ProcessMemoryUsage> {
    let mut task_info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let expected_size =
        i32::try_from(std::mem::size_of::<libc::proc_taskinfo>()).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "proc_taskinfo size exceeds c_int",
            )
        })?;
    // SAFETY: `task_info` points to writable storage of exactly
    // `expected_size` bytes. The current PID and fixed task-info flavor need no
    // additional lifetime, ownership, or thread-local guarantees.
    let returned_size = unsafe {
        libc::proc_pidinfo(
            libc::getpid(),
            libc::PROC_PIDTASKINFO,
            0,
            task_info.as_mut_ptr().cast(),
            expected_size,
        )
    };
    if returned_size == expected_size {
        // SAFETY: an exact successful `PROC_PIDTASKINFO` result initialized the
        // complete `proc_taskinfo` output object.
        let task_info = unsafe { task_info.assume_init() };
        Ok(ProcessMemoryUsage {
            virtual_size: task_info.pti_virtual_size,
            resident_size: task_info.pti_resident_size,
            faults: u64::try_from(task_info.pti_faults).unwrap_or(0),
            pageins: u64::try_from(task_info.pti_pageins).unwrap_or(0),
        })
    } else if returned_size == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "proc_pidinfo returned a partial task record",
        ))
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct TempSnapshotArtifacts {
    directory: std::path::PathBuf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl TempSnapshotArtifacts {
    fn new(name: &str) -> std::io::Result<Self> {
        let id = NEXT_HVF_TEST_FILE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let directory =
            std::env::temp_dir().join(format!("bangbang-hvf-{name}-{}-{id}", std::process::id()));
        std::fs::create_dir(&directory)?;
        Ok(Self { directory })
    }

    fn paths(&self) -> bangbang_runtime::snapshot_artifact::SnapshotArtifactPaths {
        bangbang_runtime::snapshot_artifact::SnapshotArtifactPaths::new(
            self.directory.join("state.snap"),
            self.directory.join("memory.snap"),
        )
    }

    fn assert_committed_without_staging(&self) -> std::io::Result<()> {
        let paths = self.paths();
        assert!(paths.state().is_file());
        assert!(paths.memory().is_file());
        let entries = std::fs::read_dir(&self.directory)?
            .map(|entry| Ok(entry?.file_name().to_string_lossy().into_owned()))
            .collect::<std::io::Result<Vec<_>>>()?;
        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .all(|name| !name.starts_with(".bangbang-snapshot-"))
        );
        Ok(())
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for TempSnapshotArtifacts {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
struct TempFile {
    path: std::path::PathBuf,
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl TempFile {
    fn new(name: &str, bytes: &[u8]) -> std::io::Result<Self> {
        use std::io::Write as _;

        let id = NEXT_HVF_TEST_FILE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bangbang-hvf-{name}-{}-{}", std::process::id(), id));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(bytes)?;

        Ok(Self { path })
    }

    fn new_len(name: &str, len: u64) -> std::io::Result<Self> {
        let id = NEXT_HVF_TEST_FILE_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("bangbang-hvf-{name}-{}-{}", std::process::id(), id));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.set_len(len)?;

        Ok(Self { path })
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn path_text(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn arm64_image() -> Result<Vec<u8>, &'static str> {
    const ARM64_IMAGE_HEADER_SIZE: usize = 64;
    const ARM64_IMAGE_TEXT_OFFSET_OFFSET: usize = 8;
    const ARM64_IMAGE_SIZE_OFFSET: usize = 16;
    const ARM64_IMAGE_MAGIC_OFFSET: usize = 56;
    const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;

    let mut bytes = vec![0xaa; ARM64_IMAGE_HEADER_SIZE];
    write_u64_le(&mut bytes, ARM64_IMAGE_TEXT_OFFSET_OFFSET, 0)?;
    write_u64_le(
        &mut bytes,
        ARM64_IMAGE_SIZE_OFFSET,
        ARM64_IMAGE_HEADER_SIZE as u64,
    )?;
    write_u32_le(&mut bytes, ARM64_IMAGE_MAGIC_OFFSET, ARM64_IMAGE_MAGIC)?;
    Ok(bytes)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn arm64_store_u32_and_hvc_program(target: u64, value: u32) -> Vec<u8> {
    const MOVZ_X0: u32 = 0xd280_0000;
    const MOVK_X0: u32 = 0xf280_0000;
    const MOVZ_W1: u32 = 0x5280_0001;
    const MOVK_W1_LSL_16: u32 = 0x72a0_0001;
    const STR_W1_X0: u32 = 0xb900_0001;
    const DMB_ISH: u32 = 0xd503_3bbf;
    const HVC_ZERO: u32 = 0xd400_0002;

    let mut instructions = Vec::with_capacity(9);
    instructions.push(MOVZ_X0 | u32::from(target as u16) << 5);
    for halfword in 1..4_u32 {
        let immediate = ((target >> (halfword * 16)) & u64::from(u16::MAX)) as u32;
        instructions.push(MOVK_X0 | (halfword << 21) | (immediate << 5));
    }
    instructions.push(MOVZ_W1 | u32::from(value as u16) << 5);
    instructions.push(MOVK_W1_LSL_16 | ((value >> 16) << 5));
    instructions.push(STR_W1_X0);
    instructions.push(DMB_ISH);
    instructions.push(HVC_ZERO);
    instructions
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect()
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_u64_le(bytes: &mut [u8], offset: usize, value: u64) -> Result<(), &'static str> {
    let end = offset + std::mem::size_of::<u64>();
    let destination = bytes
        .get_mut(offset..end)
        .ok_or("u64 write range should fit test image")?;
    destination.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn write_u32_le(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), &'static str> {
    let end = offset + std::mem::size_of::<u32>();
    let destination = bytes
        .get_mut(offset..end)
        .ok_or("u32 write range should fit test image")?;
    destination.copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
#[test]
fn requires_macos_apple_silicon() {
    panic!("signed HVF lifecycle tests require macOS Apple Silicon");
}
