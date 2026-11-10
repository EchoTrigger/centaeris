"""Read immutable hosted text; never resolve a capture through a workspace path."""

import hashlib

from .models import TranscriptOutputCapture, TranscriptOutputChunk


def read_output_range(event, offset, byte_length):
    capture = TranscriptOutputCapture.objects.filter(event=event, purgedAt__isnull=True).first()
    if (capture is None or capture.byteLength != byte_length
            or capture.chunkSize != 65536 or not 0 <= offset <= byte_length):
        raise ValueError("transcript capture unavailable")
    if offset == byte_length:
        return "", offset
    end = min(offset + 65536, byte_length)
    first, last = offset // capture.chunkSize, (end - 1) // capture.chunkSize
    chunks = list(capture.chunks.filter(index__gte=first, index__lte=last).order_by("index"))
    if len(chunks) != last - first + 1:
        raise ValueError("transcript capture chunk missing")
    parts = []
    for index, chunk in zip(range(first, last + 1), chunks):
        data = bytes(chunk.data)
        expected = min(capture.chunkSize, byte_length - index * capture.chunkSize)
        if (chunk.index != index or len(data) != expected
                or chunk.sha256 != "sha256:" + hashlib.sha256(data).hexdigest()):
            raise ValueError("transcript capture chunk corrupt")
        parts.append(data)
    start_in_chunk = offset % capture.chunkSize
    data = b"".join(parts)[start_in_chunk:start_in_chunk + end - offset]
    try:
        return data.decode("utf-8"), end
    except UnicodeDecodeError as error:
        # A page may end inside a codepoint; a caller may never start inside one.
        if error.reason != "unexpected end of data" or error.start == 0 or end == byte_length:
            raise ValueError("transcript capture UTF-8 boundary invalid") from error
        return data[:error.start].decode("utf-8"), offset + error.start


def purge_session_outputs(session, at):
    """Caller holds the Session lock, also used by the Runtime publisher."""
    captures = TranscriptOutputCapture.objects.filter(event__session=session)
    captures.update(purgedAt=at)
    TranscriptOutputChunk.objects.filter(capture__in=captures).delete()
