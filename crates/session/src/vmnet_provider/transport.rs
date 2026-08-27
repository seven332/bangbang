use std::fmt;
use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use bangbang_unix_stream::{UnixStreamTransport, connected_unix_stream};

use super::{
    HEADER_BYTES, ProviderEnvelope, VmnetProviderError, decode_frame, decode_header, encode_frame,
};

/// Largest permitted timeout for one complete provider frame.
pub const MAX_PROVIDER_TIMEOUT: Duration = Duration::from_secs(5);

/// Descriptor-aware exact-frame transport for the private provider protocol.
pub struct VmnetProviderTransport {
    inner: UnixStreamTransport,
    poisoned: bool,
}

impl VmnetProviderTransport {
    /// Adopts one already-connected control or data stream.
    pub fn new(stream: UnixStream, timeout: Duration) -> Result<Self, VmnetProviderError> {
        if timeout.is_zero() || timeout > MAX_PROVIDER_TIMEOUT {
            return Err(VmnetProviderError::InvalidConfiguration);
        }
        let inner = UnixStreamTransport::new(stream, timeout).map_err(VmnetProviderError::from)?;
        Ok(Self {
            inner,
            poisoned: false,
        })
    }

    /// Returns whether any prior transport failure permanently poisoned the stream.
    #[must_use]
    pub const fn is_poisoned(&self) -> bool {
        self.poisoned
    }

    /// Sends one complete frame and its exact optional transferred stream.
    pub fn send(&mut self, envelope: ProviderEnvelope) -> Result<(), VmnetProviderError> {
        if self.poisoned {
            return Err(VmnetProviderError::Poisoned);
        }
        let result = self.send_inner(envelope);
        if result.is_err() {
            self.poison();
        }
        result
    }

    /// Receives one complete frame and atomically adopts its exact optional stream.
    pub fn receive(&mut self) -> Result<ProviderEnvelope, VmnetProviderError> {
        if self.poisoned {
            return Err(VmnetProviderError::Poisoned);
        }
        let result = self.receive_inner();
        if result.is_err() {
            self.poison();
        }
        result
    }

    fn send_inner(&self, envelope: ProviderEnvelope) -> Result<(), VmnetProviderError> {
        let (frame, stream) = envelope.into_parts();
        if usize::from(frame.descriptor_count()) != usize::from(stream.is_some()) {
            return Err(VmnetProviderError::InvalidFrame);
        }
        let encoded = encode_frame(&frame)?;
        match stream.as_ref() {
            Some(stream) => {
                validate_outgoing_stream(stream)?;
                self.inner
                    .send(&encoded, &[stream.as_fd()])
                    .map_err(VmnetProviderError::from)
            }
            None => self
                .inner
                .send(&encoded, &[])
                .map_err(VmnetProviderError::from),
        }
    }

    fn receive_inner(&self) -> Result<ProviderEnvelope, VmnetProviderError> {
        let deadline = self.inner.deadline().map_err(VmnetProviderError::from)?;
        let header_range = self
            .inner
            .receive_exact_until(HEADER_BYTES, 1, deadline)
            .map_err(VmnetProviderError::from)?;
        let (header_bytes, mut descriptors) = header_range.into_parts();
        let header = decode_header(&header_bytes)?;
        if descriptors.len() != usize::from(header.descriptor_count()) {
            return Err(VmnetProviderError::InvalidFrame);
        }

        let body_range = self
            .inner
            .receive_exact_until(header.body_len(), 0, deadline)
            .map_err(VmnetProviderError::from)?;
        let (body, body_descriptors) = body_range.into_parts();
        if !body_descriptors.is_empty() {
            return Err(VmnetProviderError::InvalidFrame);
        }

        let mut encoded = header_bytes;
        encoded
            .try_reserve_exact(body.len())
            .map_err(|_| VmnetProviderError::LimitExceeded)?;
        encoded.extend_from_slice(&body);
        let frame = decode_frame(&encoded)?;
        match descriptors.pop() {
            Some(descriptor) => connected_unix_stream(descriptor)
                .map(|stream| ProviderEnvelope::with_stream(frame, stream))
                .map_err(VmnetProviderError::from),
            None => Ok(ProviderEnvelope::frame_only(frame)),
        }
    }

    fn poison(&mut self) {
        self.poisoned = true;
        self.inner.shutdown();
    }
}

impl fmt::Debug for VmnetProviderTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VmnetProviderTransport")
            .field("stream", &"<redacted>")
            .field("timeout", &"<configured>")
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

fn validate_outgoing_stream(stream: &UnixStream) -> Result<(), VmnetProviderError> {
    let duplicate = stream
        .try_clone()
        .map(OwnedFd::from)
        .map_err(|error| VmnetProviderError::Io(error.kind()))?;
    let validated = connected_unix_stream(duplicate).map_err(VmnetProviderError::from)?;
    drop(validated);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::fd::AsFd;
    use std::os::unix::net::{UnixDatagram, UnixStream};
    use std::time::Duration;

    use bangbang_unix_stream::UnixStreamTransport;

    use super::*;
    use crate::SessionId;
    use crate::vmnet_provider::{
        ControlClientEvent, ControlMessage, DataClientEvent, DataOwnerEvent, ProviderFrame,
        RealizedVmnetParameters, RequestedVmnetParameters, VmnetControlBroker, VmnetControlClient,
        VmnetDataClient, VmnetDataOwner, VmnetGeneration, VmnetInterfaceId, VmnetPacketBatch,
        VmnetPolicySlot, VmnetSequence,
    };

    const TEST_TIMEOUT: Duration = Duration::from_secs(1);

    fn session(value: u8) -> SessionId {
        SessionId::from_bytes([value; 32])
    }

    fn interface() -> VmnetInterfaceId {
        VmnetInterfaceId::new(1).expect("interface should validate")
    }

    fn generation() -> VmnetGeneration {
        VmnetGeneration::new(1).expect("generation should validate")
    }

    fn sequence(value: u64) -> VmnetSequence {
        VmnetSequence::new(value).expect("sequence should validate")
    }

    fn realized() -> RealizedVmnetParameters {
        RealizedVmnetParameters::new([2, 0, 0, 0, 0, 1], 1500, 2048)
            .expect("base parameters should validate")
            .with_batch_limits(Some(8), Some(8))
            .expect("parameters should validate")
    }

    fn hello_frame() -> ProviderFrame {
        ProviderFrame::control(session(1), None, None, sequence(1), ControlMessage::Hello)
            .expect("frame should validate")
    }

    fn started_frame() -> ProviderFrame {
        ProviderFrame::control(
            session(1),
            Some(interface()),
            Some(generation()),
            sequence(1),
            ControlMessage::Started {
                parameters: realized(),
            },
        )
        .expect("frame should validate")
    }

    #[test]
    fn ordinary_and_transferred_stream_frames_round_trip() {
        let (left, right) = UnixStream::pair().expect("pair should construct");
        let mut sender = VmnetProviderTransport::new(left, TEST_TIMEOUT).expect("sender");
        let mut receiver = VmnetProviderTransport::new(right, TEST_TIMEOUT).expect("receiver");
        sender
            .send(ProviderEnvelope::frame_only(hello_frame()))
            .expect("hello should send");
        let hello = receiver.receive().expect("hello should receive");
        assert_eq!(hello.frame(), &hello_frame());
        assert!(!hello.has_stream());

        let (data, mut peer) = UnixStream::pair().expect("data pair should construct");
        sender
            .send(ProviderEnvelope::with_stream(started_frame(), data))
            .expect("started should send");
        let received = receiver.receive().expect("started should receive");
        assert_eq!(received.frame(), &started_frame());
        assert!(received.has_stream());
        let (_frame, mut stream) = received.into_parts();
        let mut stream = stream.take().expect("stream should be present");
        peer.write_all(b"x").expect("peer should write");
        let mut byte = [0_u8; 1];
        assert_eq!(stream.read(&mut byte).expect("stream should read"), 1);
        assert_eq!(byte, *b"x");
    }

    #[test]
    fn wrong_and_excess_header_rights_poison_and_close() {
        let (left, right) = UnixStream::pair().expect("pair should construct");
        let raw = UnixStreamTransport::new(left, TEST_TIMEOUT).expect("raw sender");
        let mut receiver = VmnetProviderTransport::new(right, TEST_TIMEOUT).expect("receiver");
        let encoded = encode_frame(&started_frame()).expect("frame should encode");
        let (datagram, _peer) = UnixDatagram::pair().expect("datagram pair should construct");
        raw.send(&encoded, &[datagram.as_fd()])
            .expect("wrong right should send");
        assert!(matches!(
            receiver.receive(),
            Err(VmnetProviderError::InvalidFrame)
        ));
        assert!(receiver.is_poisoned());
        assert!(matches!(
            receiver.receive(),
            Err(VmnetProviderError::Poisoned)
        ));

        let (left, right) = UnixStream::pair().expect("pair should construct");
        let raw = UnixStreamTransport::new(left, TEST_TIMEOUT).expect("raw sender");
        let mut receiver = VmnetProviderTransport::new(right, TEST_TIMEOUT).expect("receiver");
        let encoded = encode_frame(&hello_frame()).expect("frame should encode");
        let (first, _first_peer) = UnixDatagram::pair().expect("pair should construct");
        let (second, _second_peer) = UnixDatagram::pair().expect("pair should construct");
        raw.send(&encoded, &[first.as_fd(), second.as_fd()])
            .expect("rights should send");
        assert!(matches!(
            receiver.receive(),
            Err(VmnetProviderError::InvalidFrame)
        ));
        assert!(receiver.is_poisoned());
    }

    #[test]
    fn descriptor_on_body_is_rejected() {
        let frame = ProviderFrame::control(
            session(1),
            Some(interface()),
            None,
            sequence(1),
            ControlMessage::Start {
                policy_slot: VmnetPolicySlot::Shared,
                requested: RequestedVmnetParameters::new(None, None)
                    .expect("request should validate"),
            },
        )
        .expect("frame should validate");
        let encoded = encode_frame(&frame).expect("frame should encode");
        let (header, body) = encoded.split_at(HEADER_BYTES);
        let (left, right) = UnixStream::pair().expect("pair should construct");
        let raw = UnixStreamTransport::new(left, TEST_TIMEOUT).expect("raw sender");
        let mut receiver = VmnetProviderTransport::new(right, TEST_TIMEOUT).expect("receiver");
        raw.send(header, &[]).expect("header should send");
        let (datagram, _peer) = UnixDatagram::pair().expect("pair should construct");
        raw.send(body, &[datagram.as_fd()])
            .expect("body right should send");
        assert!(matches!(
            receiver.receive(),
            Err(VmnetProviderError::InvalidFrame)
        ));
        assert!(receiver.is_poisoned());
    }

    #[test]
    fn timeout_clean_eof_and_partial_eof_are_distinct_and_terminal() {
        let (left, _right) = UnixStream::pair().expect("pair should construct");
        let mut receiver = VmnetProviderTransport::new(left, Duration::from_millis(20))
            .expect("receiver should construct");
        assert!(matches!(
            receiver.receive(),
            Err(VmnetProviderError::Timeout)
        ));
        assert!(receiver.is_poisoned());

        let (left, right) = UnixStream::pair().expect("pair should construct");
        let mut receiver = VmnetProviderTransport::new(left, TEST_TIMEOUT).expect("receiver");
        drop(right);
        assert!(matches!(
            receiver.receive(),
            Err(VmnetProviderError::Disconnected)
        ));

        let (left, mut right) = UnixStream::pair().expect("pair should construct");
        let mut receiver = VmnetProviderTransport::new(left, TEST_TIMEOUT).expect("receiver");
        right.write_all(b"prefix").expect("prefix should write");
        drop(right);
        assert!(matches!(
            receiver.receive(),
            Err(VmnetProviderError::UnexpectedEof)
        ));
        assert!(receiver.is_poisoned());
    }

    #[test]
    fn transport_and_state_complete_one_transferred_data_lifecycle() {
        let (client_socket, broker_socket) = UnixStream::pair().expect("pair should construct");
        let mut client_transport =
            VmnetProviderTransport::new(client_socket, TEST_TIMEOUT).expect("client transport");
        let mut broker_transport =
            VmnetProviderTransport::new(broker_socket, TEST_TIMEOUT).expect("broker transport");
        let mut client = VmnetControlClient::new(session(3)).expect("client should construct");
        let mut broker = VmnetControlBroker::new(session(3)).expect("broker should construct");

        client_transport
            .send(client.hello().expect("hello should emit"))
            .expect("hello should send");
        broker
            .receive(broker_transport.receive().expect("hello should receive"))
            .expect("hello should validate");
        broker_transport
            .send(broker.hello_ack().expect("ack should emit"))
            .expect("ack should send");
        client
            .receive(client_transport.receive().expect("ack should receive"))
            .expect("ack should validate");

        client_transport
            .send(
                client
                    .start(
                        interface(),
                        VmnetPolicySlot::Host,
                        RequestedVmnetParameters::new(None, None).expect("request should validate"),
                    )
                    .expect("start should emit"),
            )
            .expect("start should send");
        broker
            .receive(broker_transport.receive().expect("start should receive"))
            .expect("start should validate");
        let (owner_stream, client_stream) = UnixStream::pair().expect("data pair should construct");
        broker_transport
            .send(
                broker
                    .started(generation(), realized(), client_stream)
                    .expect("started should emit"),
            )
            .expect("started should send");
        let event = client
            .receive(client_transport.receive().expect("started should receive"))
            .expect("started should validate");
        let ControlClientEvent::Started { stream, .. } = event else {
            panic!("expected started event");
        };

        let mut data_client_transport =
            VmnetProviderTransport::new(stream, TEST_TIMEOUT).expect("data client transport");
        let mut data_owner_transport =
            VmnetProviderTransport::new(owner_stream, TEST_TIMEOUT).expect("data owner transport");
        let mut data_client =
            VmnetDataClient::new(session(3), interface(), generation(), realized())
                .expect("data client");
        let mut data_owner = VmnetDataOwner::new(session(3), interface(), generation(), realized())
            .expect("data owner");
        data_client_transport
            .send(data_client.hello().expect("hello should emit"))
            .expect("hello should send");
        assert_eq!(
            data_owner.receive(
                data_owner_transport
                    .receive()
                    .expect("hello should receive")
            ),
            Ok(DataOwnerEvent::Hello)
        );
        data_owner_transport
            .send(data_owner.hello_ack().expect("ack should emit"))
            .expect("ack should send");
        assert_eq!(
            data_client.receive(data_client_transport.receive().expect("ack should receive")),
            Ok(DataClientEvent::Ready)
        );

        let packet = [1_u8, 2, 3];
        let batch = VmnetPacketBatch::write(&[&packet]).expect("batch should validate");
        data_client_transport
            .send(data_client.write(batch).expect("write should emit"))
            .expect("write should send");
        assert!(matches!(
            data_owner.receive(
                data_owner_transport
                    .receive()
                    .expect("write should receive")
            ),
            Ok(DataOwnerEvent::Write { .. })
        ));
        data_owner_transport
            .send(data_owner.write_result(1).expect("result should emit"))
            .expect("result should send");
        assert_eq!(
            data_client.receive(
                data_client_transport
                    .receive()
                    .expect("result should receive")
            ),
            Ok(DataClientEvent::WriteComplete {
                completed_packets: 1,
            })
        );
    }

    #[test]
    fn configuration_and_debug_are_bounded_and_redacted() {
        let (left, _right) = UnixStream::pair().expect("pair should construct");
        assert!(matches!(
            VmnetProviderTransport::new(left, Duration::from_secs(6)),
            Err(VmnetProviderError::InvalidConfiguration)
        ));
        let (left, _right) = UnixStream::pair().expect("pair should construct");
        let transport = VmnetProviderTransport::new(left, TEST_TIMEOUT).expect("transport");
        let debug = format!("{transport:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("1000"));
    }
}
