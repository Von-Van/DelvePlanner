import io
import json
import unittest

from delveagent.protocol import envelope as wire
from delveagent.protocol.envelope import Envelope, ProtocolError


class EnvelopeTests(unittest.TestCase):
    def test_round_trip_is_one_line(self):
        message = Envelope("req-1", wire.TOOL_CALL, {"tool": "search_tasks", "arguments": {"query": "a\nb"}})
        data = wire.encode(message)
        self.assertEqual(data.count(b"\n"), 1)
        self.assertEqual(wire.decode(data), message)

    def test_field_names_match_the_rust_side(self):
        data = wire.encode(Envelope("a", wire.HELLO_ACK, {}))
        self.assertEqual(json.loads(data), {"v": 1, "id": "a", "type": "hello_ack", "payload": {}})

    def test_constants_match_the_rust_side(self):
        self.assertEqual(wire.PROTOCOL_VERSION, 1)
        self.assertEqual(wire.MAX_MESSAGE_BYTES, 1024 * 1024)

    def test_refuses_bad_envelopes(self):
        for line in [
            b'{"v":2,"id":"a","type":"hello","payload":{}}',
            b'{"v":1,"id":"","type":"hello","payload":{}}',
            b'{"v":1,"id":"a b","type":"hello","payload":{}}',
            b'{"v":1,"id":"a","type":"hello","payload":[]}',
            b'{"v":1,"id":"a","type":"launch","payload":{}}',
            b'{"v":1,"id":"a","type":"hello","payload":{},"x":1}',
            b"not json",
        ]:
            with self.subTest(line=line), self.assertRaises(ProtocolError):
                wire.decode(line)

    def test_direction_is_enforced(self):
        line = wire.encode(Envelope("a", wire.TOOL_CALL, {}))
        with self.assertRaises(ProtocolError):
            wire.decode(line, allowed=wire.SENT_BY_HOST)

    def test_refuses_to_encode_oversized_messages(self):
        with self.assertRaises(ProtocolError):
            wire.encode(Envelope("a", wire.TRACE_EVENT, {"pad": "x" * wire.MAX_MESSAGE_BYTES}))

    def test_read_line_limits(self):
        stream = io.BytesIO(b"short\n" + b"x" * 20 + b"\n")
        self.assertEqual(wire.read_line(stream, limit=10), b"short\n")
        with self.assertRaises(ProtocolError):
            wire.read_line(stream, limit=10)
        self.assertIsNone(wire.read_line(io.BytesIO(b"partial"), limit=10))
        self.assertIsNone(wire.read_line(io.BytesIO(b""), limit=10))


if __name__ == "__main__":
    unittest.main()
