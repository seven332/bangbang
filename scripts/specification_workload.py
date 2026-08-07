"""Strict parser for the checked guest specification-workload transcript."""

from __future__ import annotations

import re
from dataclasses import dataclass


RELEASE_BYTE = b"R"
COMPUTE_OPERATIONS = 5_000_000
COMPUTE_CHECKSUM = 8_398_723_902_783_368_615
STORAGE_BYTES = 16 * 1024 * 1024
STORAGE_BLOCK_BYTES = 4096

_MARKER_PREFIX = b"BANGBANG_SPEC_"
_HEADER = b"BANGBANG_SPEC_WORKLOAD_V1"
_READY = b"BANGBANG_SPEC_INIT_READY release_byte=82"
_SUCCESS = b"BANGBANG_SPEC_WORKLOAD_OK"
_FAILURE = re.compile(rb"BANGBANG_SPEC_WORKLOAD_FAIL phase=([a-z][a-z0-9-]{0,47})")
_COMPUTE = re.compile(
    rb"BANGBANG_SPEC_COMPUTE duration_ns=([0-9]+) operations=([0-9]+) checksum=([0-9]+)"
)
_STORAGE = re.compile(
    rb"BANGBANG_SPEC_STORAGE duration_ns=([0-9]+) bytes=([0-9]+) "
    rb"block_bytes=([0-9]+) checksum=([0-9]+)"
)
_U64_MAX = (1 << 64) - 1


class SpecificationWorkloadError(ValueError):
    """The guest transcript is not the complete checked v1 protocol."""


@dataclass(frozen=True)
class SpecificationWorkloadResult:
    compute_duration_ns: int
    compute_operations: int
    compute_checksum: int
    storage_duration_ns: int
    storage_bytes: int
    storage_block_bytes: int
    storage_checksum: int


def _u64(token: bytes, field: str) -> int:
    if token != b"0" and token.startswith(b"0"):
        raise SpecificationWorkloadError(f"{field} is not canonical decimal")
    value = int(token)
    if value > _U64_MAX:
        raise SpecificationWorkloadError(f"{field} exceeds u64")
    return value


def _marker_lines(transcript: bytes | str) -> list[bytes]:
    if isinstance(transcript, str):
        try:
            data = transcript.encode("ascii")
        except UnicodeEncodeError as error:
            raise SpecificationWorkloadError("transcript is not ASCII") from error
    elif isinstance(transcript, bytes):
        data = transcript
    else:
        raise TypeError("transcript must be bytes or str")

    markers: list[bytes] = []
    for line in data.splitlines():
        if _MARKER_PREFIX not in line:
            continue
        if not line.startswith(_MARKER_PREFIX):
            raise SpecificationWorkloadError("workload record is not line-aligned")
        markers.append(line)
    return markers


def parse_transcript(
    transcript: bytes | str,
    *,
    expected_storage_checksum: int | None = None,
) -> SpecificationWorkloadResult:
    """Parse one complete v1 run, rejecting all grammar and constant drift."""

    markers = _marker_lines(transcript)
    for marker in markers:
        failure = _FAILURE.fullmatch(marker)
        if failure is not None:
            phase = failure.group(1).decode("ascii")
            raise SpecificationWorkloadError(f"guest workload failed in phase {phase}")
        if marker.startswith(b"BANGBANG_SPEC_WORKLOAD_FAIL"):
            raise SpecificationWorkloadError("malformed guest failure record")

    if len(markers) != 5:
        raise SpecificationWorkloadError(
            f"expected exactly 5 workload records, observed {len(markers)}"
        )
    if markers[0] != _HEADER:
        raise SpecificationWorkloadError("missing or misplaced v1 header")
    if markers[1] != _READY:
        raise SpecificationWorkloadError("missing, misplaced, or drifted ready record")

    compute_match = _COMPUTE.fullmatch(markers[2])
    if compute_match is None:
        raise SpecificationWorkloadError("malformed or misplaced compute record")
    compute_duration_ns = _u64(compute_match.group(1), "compute duration")
    compute_operations = _u64(compute_match.group(2), "compute operations")
    compute_checksum = _u64(compute_match.group(3), "compute checksum")
    if compute_operations != COMPUTE_OPERATIONS:
        raise SpecificationWorkloadError("compute operation count drifted")
    if compute_checksum != COMPUTE_CHECKSUM:
        raise SpecificationWorkloadError("compute checksum drifted")

    storage_match = _STORAGE.fullmatch(markers[3])
    if storage_match is None:
        raise SpecificationWorkloadError("malformed or misplaced storage record")
    storage_duration_ns = _u64(storage_match.group(1), "storage duration")
    storage_bytes = _u64(storage_match.group(2), "storage bytes")
    storage_block_bytes = _u64(storage_match.group(3), "storage block bytes")
    storage_checksum = _u64(storage_match.group(4), "storage checksum")
    if storage_bytes != STORAGE_BYTES:
        raise SpecificationWorkloadError("storage byte count drifted")
    if storage_block_bytes != STORAGE_BLOCK_BYTES:
        raise SpecificationWorkloadError("storage block size drifted")
    if expected_storage_checksum is not None:
        if not 0 <= expected_storage_checksum <= _U64_MAX:
            raise ValueError("expected storage checksum must fit u64")
        if storage_checksum != expected_storage_checksum:
            raise SpecificationWorkloadError("storage checksum did not match the root drive")

    if markers[4] != _SUCCESS:
        raise SpecificationWorkloadError("missing or misplaced success record")

    return SpecificationWorkloadResult(
        compute_duration_ns=compute_duration_ns,
        compute_operations=compute_operations,
        compute_checksum=compute_checksum,
        storage_duration_ns=storage_duration_ns,
        storage_bytes=storage_bytes,
        storage_block_bytes=storage_block_bytes,
        storage_checksum=storage_checksum,
    )
