"""
@brief Keyed opcode masking/unmasking utilities (ground-side inverse)

Implements a small-block Feistel permutation matching the flight-side
Svc::CmdDispatcherCfg::getEventOpcode() opcode mask. The flight side may
optionally mask command opcodes before placing them into downlinked events;
the ground side inverts the mask so operators see real opcodes.

The permutation is bijective over W-bit opcodes (default W=32 for a U32
FwOpcodeType) and requires only the four 64-bit round keys configured for
the project. Disabled by default; see ConfigManager config fields
"opcode_mask_enabled" and "opcode_mask_keys".
"""

GOLDEN_RATIO_64 = 0x9E3779B97F4A7C15
UINT64_MASK = 0xFFFFFFFFFFFFFFFF


def _feistel_round(half: int, key: int, h: int, half_mask: int) -> int:
    """Feistel round function.

    Mixes a half-block with a 64-bit round key using a multiply-xor-shift
    construction. All arithmetic is modulo 2**64.

    Args:
        half: Half-block input value (h bits)
        key: 64-bit round key
        h: Half-block width in bits
        half_mask: (1 << h) - 1

    Returns:
        Mixed half-block value (h bits)
    """
    mixed = ((half * GOLDEN_RATIO_64) ^ key) & UINT64_MASK
    return (mixed ^ (mixed >> h)) & half_mask


def mask_opcode(opcode: int, keys, width: int = 32) -> int:
    """Apply the keyed Feistel mask to an opcode (flight-side forward mask).

    Provided for round-trip testing and cross-checking against the flight
    implementation; the GDS itself only needs unmask_opcode().

    Args:
        opcode: Raw opcode value (width bits)
        keys: Sequence of four 64-bit round keys
        width: Opcode width in bits (must be even; default 32 for U32)

    Returns:
        Masked opcode value (width bits)
    """
    h = width // 2
    half_mask = (1 << h) - 1
    left = (opcode >> h) & half_mask
    right = opcode & half_mask
    for key in keys:
        left, right = right, left ^ _feistel_round(right, key, h, half_mask)
    return (left << h) | right


def unmask_opcode(masked: int, keys, width: int = 32) -> int:
    """Invert the keyed Feistel mask on an opcode (ground-side inverse).

    Exact inverse of mask_opcode() for the same keys and width.

    Args:
        masked: Masked opcode value (width bits)
        keys: Sequence of four 64-bit round keys (same as used to mask)
        width: Opcode width in bits (must be even; default 32 for U32)

    Returns:
        Original (unmasked) opcode value (width bits)
    """
    h = width // 2
    half_mask = (1 << h) - 1
    left = (masked >> h) & half_mask
    right = masked & half_mask
    for key in reversed(keys):
        left, right = right ^ _feistel_round(left, key, h, half_mask), left
    return (left << h) | right
