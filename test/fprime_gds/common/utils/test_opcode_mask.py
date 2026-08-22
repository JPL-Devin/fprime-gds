"""
Tests opcode_mask util (ground-side inverse of flight opcode event mask)
"""

import random
import unittest

from fprime_gds.common.decoders.event_decoder import EventDecoder
from fprime_gds.common.models.serialize.numerical_types import U32Type
from fprime_gds.common.utils.config_manager import ConfigManager
from fprime_gds.common.utils.opcode_mask import mask_opcode, unmask_opcode

TEST_KEYS = (
    0x0123456789ABCDEF,
    0xFEDCBA9876543210,
    0xDEADBEEFCAFEF00D,
    0x0F1E2D3C4B5A6978,
)


class TestOpcodeMask(unittest.TestCase):
    def test_round_trip_edge_values(self):
        for opcode in (0, 1, 0xFFFFFFFF):
            masked = mask_opcode(opcode, TEST_KEYS)
            self.assertEqual(unmask_opcode(masked, TEST_KEYS), opcode)

    def test_round_trip_random_opcodes(self):
        rng = random.Random(1234)
        for _ in range(1000):
            opcode = rng.randrange(0, 1 << 32)
            masked = mask_opcode(opcode, TEST_KEYS)
            self.assertTrue(0 <= masked < (1 << 32))
            self.assertEqual(unmask_opcode(masked, TEST_KEYS), opcode)

    def test_bijectivity_spot_check(self):
        # A contiguous range of opcodes must map to distinct masked values
        masked_values = {mask_opcode(op, TEST_KEYS) for op in range(4096)}
        self.assertEqual(len(masked_values), 4096)

    def test_known_answer_vectors(self):
        # Cross-check vectors for the flight-side implementation
        vectors = {
            0x00000000: 0x5985B270,
            0x00000001: 0x5B635AE2,
            0x00000500: 0xF02F5F33,
            0xFFFFFFFF: 0x9FF81494,
        }
        for opcode, masked in vectors.items():
            self.assertEqual(mask_opcode(opcode, TEST_KEYS), masked)
            self.assertEqual(unmask_opcode(masked, TEST_KEYS), opcode)


class FakeTemplate:
    """Minimal template stub exposing get_args() like EventTemplate"""

    def __init__(self, args):
        self.args = args

    def get_args(self):
        return self.args


class TestEventDecoderUnmaskHook(unittest.TestCase):
    def tearDown(self):
        config = ConfigManager()
        config.set_config("opcode_mask_enabled", False)
        config.set_config("opcode_mask_keys", ())

    def test_unmask_disabled_is_noop(self):
        template = FakeTemplate([("Opcode", "", U32Type)])
        arg = U32Type(mask_opcode(0x500, TEST_KEYS))
        EventDecoder.unmask_opcode_args(template, (arg,))
        self.assertEqual(arg.val, mask_opcode(0x500, TEST_KEYS))

    def test_unmask_enabled_unmasks_opcode_args_only(self):
        config = ConfigManager()
        config.set_config("opcode_mask_enabled", True)
        config.set_config("opcode_mask_keys", TEST_KEYS)
        template = FakeTemplate(
            [("Opcode", "", U32Type), ("checksum", "", U32Type)]
        )
        opcode_arg = U32Type(mask_opcode(0x500, TEST_KEYS))
        other_arg = U32Type(42)
        EventDecoder.unmask_opcode_args(template, (opcode_arg, other_arg))
        self.assertEqual(opcode_arg.val, 0x500)
        self.assertEqual(other_arg.val, 42)


if __name__ == "__main__":
    unittest.main()
