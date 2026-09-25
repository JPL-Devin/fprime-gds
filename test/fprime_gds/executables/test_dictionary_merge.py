""" Tests for fprime_gds.executables.dictionary_merge

Real fixtures: the two hub-reference deployment dictionaries (fprime-generic-hub-reference @ a32988b2), which share the
CdhCore/ComCcsds/FileHandling/DataProducts subtopologies at identical base ids and reuse local base ids for their
deployment-local instances. Derived in-test: B2 (B with every id shifted so that shared names collide with A's while ids
do not), C (A's shared subtopologies under a third deployment name, at A's ids) and C2 (same, at new ids). Every other
case is built from small spec-complete entries so the differing field is explicit.
"""

import contextlib
import copy
import io
import json
import os
import stat
import threading
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory
from unittest import mock

from fprime_gds.common.loaders.ch_json_loader import ChJsonLoader
from fprime_gds.common.loaders.cmd_json_loader import CmdJsonLoader
from fprime_gds.common.loaders.event_json_loader import EventJsonLoader
from fprime_gds.common.loaders.pkt_json_loader import PktJsonLoader
from fprime_gds.common.utils.cleanup import globals_cleanup
from fprime_gds.executables import dictionary_merge
from fprime_gds.executables.dictionary_merge import (
    UNIQUE_SECTIONS,
    LoadedInput,
    MergeOptions,
    merge_all,
    merge_dictionaries,
)

RESOURCES = Path(__file__).resolve().parent / "resources" / "dictionary_merge"
A_PATH = RESOURCES / "DeploymentATopologyDictionary.json"
B_PATH = RESOURCES / "DeploymentBTopologyDictionary.json"
GROUND_PATH = RESOURCES / "GroundChannels.json"
GROUND_MINIMAL_PATH = RESOURCES / "GroundChannelsMinimal.json"
GOLDEN_GROUND = RESOURCES / "expected" / "ground_channels_merged.json"

COUNTED = ["commands", "events", "telemetryChannels"]
B2_SHIFT = 0x20000000
C2_SHIFT = 0x40000000

# Message fingerprints (see the design's message catalogue)
E1 = "with different opcodes; use --prefer-primary", "with different ids; use --prefer-primary"
E2 = "has different definitions in"
E3 = "is used by"
E4 = "has inconsistent definitions in"
E5 = "Inconsistent metadata values"
E6 = "references unknown channel"
E7 = "Malformed dictionary '"
E8 = "Malformed dictionary section"
E10 = "cannot rename", "collides with the already-namespaced entry"
E11 = "cannot be used as a namespace prefix", "metadata.deploymentName is missing or not a string"
E12 = "is defined twice with different definitions"
E15 = "both have deploymentName ending in"
W1 = "dropped in favour of"
W2 = "; renamed to '"
W2P = "name already namespaced as"
W3 = "kept definition '"
W4 = "— '"
W5 = "kept definition from"
W6 = "libraryVersions differ"
W7 = "removed because it references dropped channel"
W8 = "removed from omitted list"
W9 = "re-run the merge N-way"
W9_SUFFIX = "is appended un-prefixed although"
W9_MIRROR = "is appended although un-prefixed"
W10 = "in omitted (kept; the GDS ignores omitted)"
W11 = "select it with --packet-set-name"


def load(path):
    with open(path, "r") as file_handle:
        return json.load(file_handle)


def as_set(entries):
    return {json.dumps(entry, sort_keys=True) for entry in entries}


def count(lines, *fingerprints):
    return sum(1 for line in lines if any(fingerprint in line for fingerprint in fingerprints))


def errors(lines):
    return [line for line in lines if line.startswith("[ERROR]") and "Merge failed" not in line]


def warnings(lines):
    return [line for line in lines if line.startswith("[WARNING]")]


def type_of(name="U32", size=32):
    return {"name": name, "kind": "integer", "size": size, "signed": False}


def command(name, opcode, **kwargs):
    return {"name": name, "commandKind": "async", "opcode": opcode, "formalParams": [], "queueFullBehavior": "assert",
            "annotation": "", **kwargs}


def event(name, id, fmt="x", **kwargs):
    return {"name": name, "severity": "ACTIVITY_LO", "formalParams": [], "id": id, "format": fmt, "annotation": "",
            **kwargs}


def channel(name, id, **kwargs):
    return {"name": name, "type": type_of(), "id": id, "telemetryUpdate": "always", "annotation": "", **kwargs}


def parameter(name, id, **kwargs):
    return {"name": name, "type": type_of(), "id": id, "annotation": "", **kwargs}


def record(name, id, **kwargs):
    return {"name": name, "type": type_of(), "array": False, "id": id, "annotation": "", **kwargs}


def container(name, id, **kwargs):
    return {"name": name, "id": id, "defaultPriority": 1, "annotation": "", **kwargs}


def enum_type(qualified_name, enumerators):
    return {"kind": "enum", "qualifiedName": qualified_name, "representationType": type_of("U16", 16),
            "enumeratedConstants": [{"name": n, "value": v, "annotation": ""} for n, v in enumerators]}


def alias_type(qualified_name, width):
    return {"kind": "alias", "qualifiedName": qualified_name, "type": type_of(f"U{width}", width)}


def constant(qualified_name, value):
    return {"kind": "constant", "qualifiedName": qualified_name, "type": type_of(), "value": value, "annotation": ""}


def packet(name, id, members, group=1):
    return {"name": name, "id": id, "group": group, "members": list(members)}


def packet_set(name, packets, omitted=()):
    return {"name": name, "members": list(packets), "omitted": list(omitted)}


def make_dictionary(deployment="Ref.Ref", *, commands=(), events=(), channels=(), parameters=(), records=(),
                    containers=(), types=(), constants=(), packet_sets=(), **metadata):
    """ A spec-complete dictionary with all ten sections """
    meta = {"deploymentName": deployment, "projectVersion": "p1", "frameworkVersion": "f1", "libraryVersions": [],
            "dictionarySpecVersion": "1.0.0"}
    meta.update(metadata)
    for key in [key for key, value in metadata.items() if value is None]:
        del meta[key]
    return {
        "metadata": meta,
        "typeDefinitions": list(types),
        "constants": list(constants),
        "commands": list(commands),
        "parameters": list(parameters),
        "events": list(events),
        "telemetryChannels": list(channels),
        "records": list(records),
        "containers": list(containers),
        "telemetryPacketSets": list(packet_sets),
    }


def shift_ids(dictionary, offset):
    result = copy.deepcopy(dictionary)
    for section, id_key in UNIQUE_SECTIONS.items():
        for entry in result[section]:
            entry[id_key] += offset
    return result


def derive_b2(b_dict):
    return shift_ids(b_dict, B2_SHIFT)


def derive_c(a_dict, b_dict, offset=0, deployment="FprimeGenericHubReference.DeploymentC.DeploymentC"):
    """ A's shared-subtopology entries only (names present in both A and B), under a third deployment name """
    result = copy.deepcopy(a_dict)
    for section in UNIQUE_SECTIONS:
        shared = {entry["name"] for entry in b_dict[section]}
        result[section] = [entry for entry in result[section] if entry["name"] in shared]
    result["metadata"]["deploymentName"] = deployment
    return shift_ids(result, offset)


def merge(*dictionaries, **options):
    """ Merge parsed dictionaries in memory; returns (merged or None, report, merger) """
    inputs = [LoadedInput(index, f"d{index}", copy.deepcopy(dictionary))
              for index, dictionary in enumerate(dictionaries, start=1)]
    return merge_all(inputs, MergeOptions(**options))


def merge_ok(*dictionaries, **options):
    merged, report, _ = merge(*dictionaries, **options)
    assert merged is not None, "\n".join(report.errors)
    return merged, report


def merge_fails(*dictionaries, **options):
    merged, report, _ = merge(*dictionaries, **options)
    assert merged is None, "expected the merge to fail"
    return report


class DictionaryMergeTestCase(unittest.TestCase):
    """ Shared fixtures: real A and B, derived B2/C/C2, and a CLI runner """

    @classmethod
    def setUpClass(cls):
        cls.A = load(A_PATH)
        cls.B = load(B_PATH)
        cls.B2 = derive_b2(cls.B)
        cls.C = derive_c(cls.A, cls.B)
        cls.C2 = derive_c(cls.A, cls.B, C2_SHIFT)
        cls.M, _ = merge_ok(cls.A, cls.B2)
        cls.shared_names = {section: {e["name"] for e in cls.A[section]} & {e["name"] for e in cls.B[section]}
                            for section in UNIQUE_SECTIONS}

    def setUp(self):
        self._tmp = TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def write(self, name, dictionary):
        path = self.tmp / name
        with open(path, "w") as file_handle:
            json.dump(dictionary, file_handle, indent=2)
        return path

    def run_cli(self, *args, output=None):
        """ Run main(); returns (exit code, stderr lines, output path) """
        output = self.tmp / "out.json" if output is None else output
        stderr = io.StringIO()
        with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as context:
            dictionary_merge.main([str(arg) for arg in args] + ["--output", str(output)])
        return context.exception.code, stderr.getvalue().splitlines(), output

    def assertCounts(self, merged, commands, events, channels):
        self.assertEqual([len(merged[s]) for s in COUNTED], [commands, events, channels])

    def prefixed_names(self, merged, prefixes=("DeploymentA.", "DeploymentB.", "DeploymentC.")):
        return [e["name"] for s in UNIQUE_SECTIONS for e in merged[s] if e["name"].startswith(prefixes)]


class TestFeatureOffCompatibility(DictionaryMergeTestCase):

    def test_feature_off_byte_identical(self):
        code, lines, output = self.run_cli("--permissive", A_PATH, GROUND_PATH)
        self.assertEqual(code, 0)
        self.assertEqual(lines, [])
        self.assertEqual(output.read_bytes(), GOLDEN_GROUND.read_bytes())
        # --name is usable (dotted identifier) and unknown top-level keys keep today's order/precedence
        d1 = make_dictionary("Ref.One", commands=[command("Ref.a.CMD", 1)])
        d2 = make_dictionary("Ref.Two", commands=[command("Ref.b.CMD", 2)])
        d1["extra"], d2["extra"], d2["only2"] = "one", "two", True
        merged, _ = merge_ok(d1, d2, name="X.Y")
        self.assertEqual(merged["metadata"]["deploymentName"], "X.Y")
        self.assertEqual(merged["extra"], "one")
        self.assertEqual(list(merged), list({**d2, **d1}))

    def test_ground_channels_minimal_metadata(self):
        code, lines, output = self.run_cli("--permissive", A_PATH, GROUND_MINIMAL_PATH)
        self.assertEqual((code, lines), (0, []))
        # identical to the GroundChannels golden except for the missing deploymentName, which reads 'unknown'
        expected = GOLDEN_GROUND.read_text().replace("DeploymentA_GroundChannels_merged", "DeploymentA_unknown_merged")
        self.assertEqual(output.read_text(), expected)
        code, lines, _ = self.run_cli(A_PATH, GROUND_MINIMAL_PATH)
        self.assertEqual(code, 1)
        self.assertIn("(a32988b vs None)", "\n".join(lines))
        # a colliding channel name needs a prefix, which this input cannot provide
        ground = load(GROUND_MINIMAL_PATH)
        ground["telemetryChannels"][0]["name"] = self.A["telemetryChannels"][0]["name"]
        report = merge_fails(self.A, ground, permissive=True)
        self.assertEqual(count(report.errors, E11[1]), 1)

    def test_ground_channels_howto(self):
        ground = load(GROUND_PATH)
        merge_ok(self.A, ground, permissive=True)
        self.assertEqual(count(merge_fails(self.A, ground).errors, E5), 2)
        colliding = copy.deepcopy(ground)
        colliding["telemetryChannels"][0]["name"] = "CdhCore.cmdDisp.CommandsDispatched"
        merged, report = merge_ok(self.A, colliding, permissive=True)
        names = {e["name"] for e in merged["telemetryChannels"]}
        self.assertIn("DeploymentA.CdhCore.cmdDisp.CommandsDispatched", names)
        self.assertIn("GroundChannels.CdhCore.cmdDisp.CommandsDispatched", names)
        self.assertNotIn("CdhCore.cmdDisp.CommandsDispatched", names)
        self.assertEqual(count(report.warnings, W2), 1)
        report = merge_fails(self.A, colliding, permissive=True, no_namespace=True)
        self.assertEqual(count(report.errors, *E1), 1)

    def test_equal_duplicate_keeps_main_spelling(self):
        respelled = make_dictionary(self.A["metadata"]["deploymentName"],
                                    types=copy.deepcopy(self.A["typeDefinitions"]),
                                    constants=copy.deepcopy(self.A["constants"]), projectVersion="a32988b",
                                    frameworkVersion="v4.2.0")
        for entry in respelled["constants"]:
            if isinstance(entry["value"], int):
                entry["value"] = float(entry["value"])
        code, lines, output = self.run_cli(A_PATH, self.write("t.json", respelled))
        self.assertEqual((code, lines), (0, []))
        text = output.read_text()
        self.assertIn('"value": 65535,', text)
        self.assertNotIn("65535.0", text)
        self.assertEqual(load(output)["constants"], self.A["constants"])


class TestRealHubReference(DictionaryMergeTestCase):

    def test_identical_shared_entries_dedupe(self):
        report = merge_fails(self.A, self.B)
        self.assertEqual(len(report.errors), 85)
        self.assertEqual(count(report.errors, E3), 85)
        self.assertEqual(report.warnings, [])
        merged, report = merge_ok(self.A, self.B, prefer_primary=True)
        for section in list(UNIQUE_SECTIONS) + ["typeDefinitions", "constants"]:
            self.assertEqual(merged[section], self.A[section], section)
        self.assertEqual(self.prefixed_names(merged), [])

    def test_different_name_same_id(self):
        code, lines, output = self.run_cli(A_PATH, B_PATH)
        self.assertEqual(code, 1)
        self.assertFalse(output.exists())
        by_section = [count(errors(lines), f"] {section}:") for section in COUNTED]
        self.assertEqual(by_section, [10, 38, 37])
        self.assertTrue(lines[0].startswith(
            "[ERROR] commands: opcode 0x600 is used by 'FprimeGenericHubReference.DeploymentA.a_cmdSeq.CS_RUN' in '"))
        self.assertIn("Merge failed with 85 error(s) and 0 warning(s); no output written", lines[-1])
        code, lines, output = self.run_cli("--prefer-primary", "--name", "FprimeGenericHubReference.Hub",
                                           A_PATH, B_PATH)
        self.assertEqual(code, 0)
        self.assertEqual(count(lines, W4), 85)
        self.assertEqual(count(lines, W2), 0)
        merged = load(output)
        self.assertCounts(merged, 47, 214, 97)
        self.assertEqual(merged["metadata"]["deploymentName"], "FprimeGenericHubReference.Hub")
        hub_commands = [c for c in merged["commands"] if c["opcode"] == 0x11017500]
        self.assertEqual([c["name"] for c in hub_commands],
                         ["FprimeGenericHubReference.DeploymentA.c_comp.HubCommandTest"])
        # B as main: A's non-colliding locals are appended
        merged, report = merge_ok(self.B, self.A, prefer_primary=True)
        self.assertEqual(count(report.warnings, W4), 85)
        self.assertCounts(merged, 47, 214, 97)
        self.assertNotEqual(merged["commands"], self.B["commands"])
        # --no-namespace changes nothing here: there is no same-name/different-id pair
        self.assertEqual(count(merge_fails(self.A, self.B, no_namespace=True).errors, E3), 85)

    def test_same_name_different_id_default_renames_both(self):
        code, lines, output = self.run_cli(A_PATH, self.write("B2.json", self.B2))
        self.assertEqual(code, 0)
        self.assertEqual(len(warnings(lines)), 261)
        self.assertEqual(count(lines, W2), 260)
        self.assertEqual(lines[-1], "[WARNING] Merged 2 dictionaries with 260 warning(s): 260 renamed")
        merged = load(output)
        self.assertCounts(merged, 93, 419, 191)
        self.assertEqual(len(self.prefixed_names(merged)), 520)
        names = {section: [e["name"] for e in merged[section]] for section in UNIQUE_SECTIONS}
        for section in UNIQUE_SECTIONS:
            self.assertFalse(self.shared_names[section] & set(names[section]), section)
            for shared in self.shared_names[section]:
                self.assertIn(f"DeploymentA.{shared}", names[section])
                self.assertIn(f"DeploymentB.{shared}", names[section])
        opcodes = {c["name"]: c["opcode"] for c in merged["commands"]}
        self.assertEqual(opcodes["DeploymentA.CdhCore.cmdDisp.CMD_NO_OP"], 0x1000000)
        self.assertEqual(opcodes["DeploymentB.CdhCore.cmdDisp.CMD_NO_OP"], 0x21000000)
        # A's entries keep their slots, B2's follow in B2's order; ids untouched
        self.assertEqual([c["opcode"] for c in merged["commands"][:47]], [c["opcode"] for c in self.A["commands"]])
        self.assertEqual(count(lines, W9), 0)
        first = [line for line in lines if "CdhCore.cmdDisp.CMD_NO_OP" in line][0]
        self.assertIn("has opcode 0x1000000 in", first)
        self.assertIn("renamed to 'DeploymentA.CdhCore.cmdDisp.CMD_NO_OP' and 'DeploymentB.CdhCore.cmdDisp.CMD_NO_OP'",
                      first)
        # --prefer-primary does not change a same-name/different-id outcome
        merged2, report = merge_ok(self.A, self.B2, prefer_primary=True)
        self.assertEqual(merged2, merged)
        self.assertEqual(count(report.warnings, W2), 260)

    def test_no_namespace_restores_error(self):
        code, lines, output = self.run_cli("--no-namespace", A_PATH, self.write("B2.json", self.B2))
        self.assertEqual(code, 1)
        self.assertFalse(output.exists())
        self.assertEqual(count(errors(lines), *E1), 260)
        self.assertEqual(len(errors(lines)), 260)
        d1 = make_dictionary("Ref.One", commands=[command("Ref.a.CMD", 1)])
        d2 = make_dictionary("Ref.Two", commands=[command("Ref.a.CMD", 2)])
        report = merge_fails(d1, d2, no_namespace=True)
        self.assertEqual(report.errors, [
            "commands: 'Ref.a.CMD' is defined in 'd1' (opcode 0x1) and 'd2' (opcode 0x2) with different opcodes; use "
            "--prefer-primary to keep the first, or drop --no-namespace to keep both under '<prefix>.'-qualified "
            "names"])

    def test_no_namespace_prefer_primary_drops(self):
        merged, report = merge_ok(self.A, self.B2, no_namespace=True, prefer_primary=True)
        self.assertEqual(count(report.warnings, W1), 260)
        self.assertEqual(len(report.warnings), 260)
        self.assertCounts(merged, 57, 252, 134)
        self.assertEqual(self.prefixed_names(merged), [])
        opcodes = {c["name"]: c["opcode"] for c in merged["commands"]}
        self.assertEqual(opcodes["CdhCore.cmdDisp.CMD_NO_OP"], 0x1000000)
        self.assertEqual(opcodes["FprimeGenericHubReference.DeploymentB.b_cmdSeq.CS_RUN"], 0x20000600)

    def test_remerge_with_own_input_is_noop(self):
        for other in (self.B2, self.A):
            merged, report = merge_ok(self.M, other)
            self.assertEqual(report.warnings, [])
            for section in list(UNIQUE_SECTIONS) + ["typeDefinitions", "constants", "telemetryPacketSets"]:
                self.assertEqual(merged[section], self.M[section], section)

    def test_chained_merge_is_not_nway(self):
        report = merge_fails(self.M, self.C)
        self.assertEqual(count(report.errors, E3), 260)
        merged, report = merge_ok(self.M, self.C2)
        self.assertEqual(count(report.warnings, W9_SUFFIX), 260)
        self.assertEqual(count(report.warnings, W2), 0)
        self.assertCounts(merged, 129, 586, 248)
        bare = [c for c in merged["commands"] if c["name"] == "CdhCore.cmdDisp.CMD_NO_OP"]
        self.assertEqual([c["opcode"] for c in bare], [0x41000000])
        nway, _ = merge_ok(self.A, self.B2, self.C2)
        self.assertNotEqual(as_set(merged["commands"]), as_set(nway["commands"]))
        merged, report = merge_ok(self.M, self.C, prefer_primary=True)
        self.assertEqual(count(report.warnings, W4), 260)

    def test_no_namespace_remerge_packet_set(self):
        b2 = copy.deepcopy(self.B2)
        b2["telemetryPacketSets"] = [packet_set("Pkts", [packet("P1", 1, ["CdhCore.cmdDisp.CommandsDispatched"]),
                                                        packet("P2", 2, [b2["telemetryChannels"][0]["name"]])])]
        report = merge_fails(self.M, b2, no_namespace=True)
        self.assertEqual(count(report.errors, E3), 260)
        self.assertEqual(count(report.errors, E2), 0)
        merged, report = merge_ok(self.M, b2, no_namespace=True, prefer_primary=True)
        self.assertEqual(count(report.warnings, W4), 260)
        self.assertEqual(count(report.warnings, W7), 1)
        self.assertEqual(count(report.warnings, W3), 0)
        for section in UNIQUE_SECTIONS:
            self.assertEqual(merged[section], self.M[section], section)
        self.assertEqual([p["name"] for p in merged["telemetryPacketSets"][0]["members"]], ["P2"])

    def test_third_input_matches_renamed_original(self):
        c = copy.deepcopy(self.C)
        c["telemetryPacketSets"] = [packet_set("Pkts", [packet("P1", 1, ["CdhCore.cmdDisp.CommandsDispatched"])],
                                              omitted=["CdhCore.cmdDisp.CommandErrors"])]
        merged, report, merger = merge(self.A, self.B2, c)
        self.assertIsNotNone(merged, report.errors)
        self.assertEqual(count(report.warnings, W2), 260)
        self.assertEqual(len(report.warnings), 260)
        for section in UNIQUE_SECTIONS:
            self.assertEqual(merged[section], self.M[section], section)
        self.assertEqual(merger.rename_map[3]["telemetryChannels"]["CdhCore.cmdDisp.CommandsDispatched"],
                         "DeploymentA.CdhCore.cmdDisp.CommandsDispatched")
        packets = merged["telemetryPacketSets"][0]
        self.assertEqual(packets["members"][0]["members"], ["DeploymentA.CdhCore.cmdDisp.CommandsDispatched"])
        self.assertEqual(packets["omitted"], ["DeploymentA.CdhCore.cmdDisp.CommandErrors"])

    def test_third_input_own_id_gets_own_prefix(self):
        merged, report = merge_ok(self.A, self.B2, self.C2)
        self.assertEqual(count(report.warnings, W2), 260)
        self.assertEqual(count(report.warnings, W2P), 260)
        self.assertCounts(merged, 129, 586, 248)
        opcodes = {c["name"]: c["opcode"] for c in merged["commands"]}
        self.assertEqual(opcodes["DeploymentC.CdhCore.cmdDisp.CMD_NO_OP"], 0x41000000)
        line = [w for w in report.warnings if W2P in w and "CMD_NO_OP" in w][0]
        self.assertIn("'DeploymentA.CdhCore.cmdDisp.CMD_NO_OP', 'DeploymentB.CdhCore.cmdDisp.CMD_NO_OP'", line)

    def test_third_input_order_independent(self):
        for third in (self.C, self.C2):
            merged1, _, merger1 = merge(self.A, self.B2, third)
            merged2, _, merger2 = merge(self.A, third, self.B2)
            for section in UNIQUE_SECTIONS:
                self.assertEqual(as_set(merged1[section]), as_set(merged2[section]), section)
            self.assertEqual(merger1.rename_map[1], merger2.rename_map[1])

    def test_equal_prefixes_error(self):
        c3 = copy.deepcopy(self.C2)
        c3["metadata"]["deploymentName"] = "FprimeGenericHubReference.DeploymentB.DeploymentB"
        report = merge_fails(self.A, self.B2, c3)
        self.assertEqual(count(report.errors, E15), 260)
        self.assertEqual(len(report.errors), 260)
        # without a collision the shared last segment is harmless
        d1 = make_dictionary("X.Same", commands=[command("Ref.a.CMD", 1)])
        d2 = make_dictionary("Y.Same", commands=[command("Ref.b.CMD", 2)])
        merge_ok(d1, d2)
        # with a collision it is reported once per colliding name and nothing is renamed
        d2["commands"] = [command("Ref.a.CMD", 2)]
        report = merge_fails(d1, d2)
        self.assertEqual(count(report.errors, E15), 1)
        self.assertIn("would share the prefix 'Same.'", report.errors[0])
        # single-segment names are their own prefix
        merged, _ = merge_ok(make_dictionary("One", commands=[command("Ref.a.CMD", 1)]),
                             make_dictionary("Two", commands=[command("Ref.a.CMD", 2)]))
        self.assertEqual([c["name"] for c in merged["commands"]], ["One.Ref.a.CMD", "Two.Ref.a.CMD"])

    def test_w9_not_emitted_on_nway(self):
        runs = [((self.A, self.B2), {}, True), ((self.A, self.B), {}, False), ((self.A, self.B2, self.C), {}, True),
                ((self.A, self.B2, self.C2), {}, True), ((self.A, self.B), {"prefer_primary": True}, True),
                ((self.B, self.A), {"prefer_primary": True}, True),
                ((self.A, load(GROUND_PATH)), {"permissive": True}, True)]
        for dictionaries, options, succeeds in runs:
            merged, report, _ = merge(*dictionaries, **options)
            self.assertEqual(merged is not None, succeeds, report.errors)
            self.assertEqual(count(report.warnings, W9), 0)
        nested = make_dictionary("Ref.Z", commands=[command("DeploymentZ.CdhCore.cmdDisp.CMD_NO_OP", 0x7000000)],
                                 projectVersion="a32988b", frameworkVersion="v4.2.0")
        _, report = merge_ok(self.A, nested)
        self.assertEqual(count(report.warnings, W9_MIRROR), 1)
        self.assertIn("un-prefixed 'CdhCore.cmdDisp.CMD_NO_OP' exists", report.warnings[0])

    def test_determinism(self):
        b2 = self.write("B2.json", self.B2)
        first = self.run_cli(A_PATH, b2, output=self.tmp / "one.json")
        second = self.run_cli(A_PATH, b2, output=self.tmp / "two.json")
        self.assertEqual(first[1], second[1])
        self.assertEqual(first[2].read_bytes(), second[2].read_bytes())

    def test_output_loads_in_gds_loaders(self):
        b2 = copy.deepcopy(self.B2)
        b2["telemetryPacketSets"] = [packet_set("Pkts", [packet("P1", 1, ["CdhCore.cmdDisp.CommandsDispatched"])])]
        code, _, output = self.run_cli(A_PATH, self.write("B2.json", b2))
        self.assertEqual(code, 0)
        globals_cleanup()
        self.addCleanup(globals_cleanup)
        cmd_ids, cmd_names, _ = CmdJsonLoader(str(output)).construct_dicts(str(output))
        evr_ids, _, _ = EventJsonLoader(str(output)).construct_dicts(str(output))
        ch_ids, ch_names, _ = ChJsonLoader(str(output)).construct_dicts(str(output))
        merged = load(output)
        self.assertEqual(len(cmd_ids), len(merged["commands"]))
        self.assertEqual(len(evr_ids), len(merged["events"]))
        self.assertEqual(len(ch_ids), len(merged["telemetryChannels"]))
        self.assertEqual(cmd_names["DeploymentA.CdhCore.cmdDisp.CMD_NO_OP"].get_op_code(), 0x1000000)
        self.assertEqual(cmd_names["DeploymentB.CdhCore.cmdDisp.CMD_NO_OP"].get_op_code(), 0x21000000)
        self.assertNotIn("CdhCore.cmdDisp.CMD_NO_OP", cmd_names)
        self.assertNotIn("CdhCore.cmdDisp.CommandsDispatched", ch_names)
        self.assertEqual(cmd_names["DeploymentB.CdhCore.cmdDisp.CMD_NO_OP"].get_comp_name(),
                         "DeploymentB.CdhCore.cmdDisp")
        _, pkt_names, _ = PktJsonLoader(str(output)).construct_dicts("Pkts", ch_names)
        self.assertEqual([ch.get_full_name() for ch in pkt_names["P1"].get_ch_list()],
                         ["DeploymentB.CdhCore.cmdDisp.CommandsDispatched"])


class TestSyntheticConflicts(DictionaryMergeTestCase):

    def test_same_name_different_id_body_differs_is_still_rename(self):
        d1 = make_dictionary("Ref.One", channels=[channel("Sub.c.X", 1, format="{} s")])
        d2 = make_dictionary("Ref.Two", channels=[channel("Sub.c.X", 2, format="{} ms")])
        merged, report = merge_ok(d1, d2)
        self.assertEqual([c["name"] for c in merged["telemetryChannels"]], ["One.Sub.c.X", "Two.Sub.c.X"])
        self.assertEqual(len(report.warnings), 1)
        self.assertIn(W2, report.warnings[0])

    def test_prefer_primary_default_rename_wins(self):
        d1 = make_dictionary("Ref.One", commands=[command("Sub.c.GO", 1), command("Ref.one.LOCAL", 2)])
        d2 = make_dictionary("Ref.Two", commands=[command("Sub.c.GO", 3), command("Ref.two.LOCAL", 2)])
        merged, report = merge_ok(d1, d2, prefer_primary=True)
        self.assertEqual([c["name"] for c in merged["commands"]], ["One.Sub.c.GO", "Ref.one.LOCAL", "Two.Sub.c.GO"])
        self.assertEqual(count(report.warnings, W2), 1)
        self.assertEqual(count(report.warnings, W4), 1)

    def test_same_name_different_body_same_id(self):
        d1 = make_dictionary("Ref.One", events=[event("Sub.c.E", 1, "a")])
        d2 = make_dictionary("Ref.Two", events=[event("Sub.c.E", 1, "b")])
        self.assertEqual(count(merge_fails(d1, d2).errors, E2), 1)
        self.assertEqual(count(merge_fails(d1, d2, no_namespace=True).errors, E2), 1)
        merged, report = merge_ok(d1, d2, prefer_primary=True)
        self.assertEqual(merged["events"], d1["events"])
        self.assertEqual(count(report.warnings, W3), 1)
        # same after main's entry was renamed by an earlier collision (orig_name test)
        d2 = make_dictionary("Ref.Two", events=[event("Sub.c.E", 2, "a")])
        d3 = make_dictionary("Ref.Three", events=[event("Sub.c.E", 1, "b")])
        report = merge_fails(d1, d2, d3)
        self.assertEqual(count(report.errors, E2), 1)
        merged, report = merge_ok(d1, d2, d3, prefer_primary=True)
        self.assertEqual([e["name"] for e in merged["events"]], ["One.Sub.c.E", "Two.Sub.c.E"])
        self.assertIn("kept definition 'One.Sub.c.E'", report.warnings[-1])

    def test_prefer_primary_body_conflict_packet_refs(self):
        d1 = make_dictionary("Ref.One", channels=[channel("Sub.c.X", 1)])
        d2 = make_dictionary("Ref.Two", channels=[channel("Sub.c.X", 1, format="{}")],
                             packet_sets=[packet_set("Pkts", [packet("P", 1, ["Sub.c.X"])])])
        merged, report, merger = merge(d1, d2, prefer_primary=True)
        self.assertIsNotNone(merged)
        self.assertEqual(count(report.warnings, W7), 0)
        self.assertEqual(merged["telemetryPacketSets"][0]["members"][0]["members"], ["Sub.c.X"])
        self.assertEqual(merger.dropped[2]["telemetryChannels"], set())
        # main's X renamed by an earlier collision: d3's references follow the kept name
        d2b = make_dictionary("Ref.Two", channels=[channel("Sub.c.X", 2)])
        d3 = make_dictionary("Ref.Three", channels=[channel("Sub.c.X", 1, format="{}")],
                             packet_sets=[packet_set("Pkts", [packet("P", 1, ["Sub.c.X"])])])
        merged, report = merge_ok(d1, d2b, d3, prefer_primary=True)
        self.assertEqual(merged["telemetryPacketSets"][0]["members"][0]["members"], ["One.Sub.c.X"])

    def test_id_zero_is_checked(self):
        d1 = make_dictionary("Ref.One", events=[event("Ref.a.E", 0)])
        d2 = make_dictionary("Ref.Two", events=[event("Ref.b.E", 0)])
        self.assertEqual(count(merge_fails(d1, d2).errors, E3), 1)

    def test_type_conflict(self):
        time_base = enum_type("TimeBase", [("TB_NONE", 0), ("TB_PROC_TIME", 1)])
        extended = enum_type("TimeBase", [("TB_NONE", 0), ("TB_PROC_TIME", 1), ("TB_EXTRA", 2)])
        d1 = make_dictionary("Ref.One", types=[time_base, alias_type("Ref.Alias", 32)])
        d2 = make_dictionary("Ref.Two", types=[extended, alias_type("Ref.Alias", 32)])
        report = merge_fails(d1, d2)
        self.assertEqual(report.errors, ["typeDefinitions: 'TimeBase' has inconsistent definitions in 'd1' and 'd2'; "
                                         "use --prefer-primary to keep the earliest"])
        merged, report = merge_ok(d1, d2, prefer_primary=True)
        self.assertEqual(merged["typeDefinitions"], d1["typeDefinitions"])
        self.assertEqual(count(report.warnings, W5), 1)
        merged, _ = merge_ok(d1, make_dictionary("Ref.Two", types=[alias_type("Ref.Alias", 32)]))
        self.assertEqual(merged["typeDefinitions"], d1["typeDefinitions"])

    def test_constant_conflict(self):
        d1 = make_dictionary("Ref.One", constants=[constant("Ref.K", 1)])
        d2 = make_dictionary("Ref.Two", constants=[constant("Ref.K", 2)])
        self.assertEqual(count(merge_fails(d1, d2).errors, E4), 1)
        merged, report = merge_ok(d1, d2, prefer_primary=True)
        self.assertEqual(merged["constants"], d1["constants"])
        self.assertEqual(count(report.warnings, W5), 1)

    def test_records_containers_follow_unique_rules(self):
        d1 = make_dictionary("Ref.One", records=[record("Sub.r.R", 1)], containers=[container("Sub.r.C", 1)])
        same = make_dictionary("Ref.Two", records=[record("Sub.r.R", 1)], containers=[container("Sub.r.C", 1)])
        merged, report = merge_ok(d1, same)
        self.assertEqual((merged["records"], merged["containers"], report.warnings),
                         (d1["records"], d1["containers"], []))
        shifted = make_dictionary("Ref.Two", records=[record("Sub.r.R", 2)], containers=[container("Sub.r.C", 2)])
        merged, report = merge_ok(d1, shifted)
        self.assertEqual([r["name"] for r in merged["records"]], ["One.Sub.r.R", "Two.Sub.r.R"])
        self.assertEqual([c["name"] for c in merged["containers"]], ["One.Sub.r.C", "Two.Sub.r.C"])
        body = make_dictionary("Ref.Two", records=[record("Sub.r.R", 1, array=True)],
                               containers=[container("Sub.r.C", 1)])
        self.assertEqual(count(merge_fails(d1, body).errors, E2), 1)
        other = make_dictionary("Ref.Two", records=[record("Sub.q.R", 1)], containers=[container("Sub.q.C", 1)])
        self.assertEqual(count(merge_fails(d1, other).errors, E3), 2)

    def test_second_namespace_collides(self):
        d1 = make_dictionary("Ref.P", commands=[command("Ref.a.X", 1), command("P.Ref.a.X", 3)])
        d2 = make_dictionary("Ref.Q", commands=[command("Ref.a.X", 2)])
        report = merge_fails(d1, d2)
        self.assertEqual(count(report.errors, E10[0]), 1)
        self.assertIn("cannot rename 'Ref.a.X' from 'd1' to 'P.Ref.a.X'", report.errors[0])
        # the arriving entry's own target is held
        d1 = make_dictionary("Ref.P", commands=[command("Ref.a.X", 1), command("Q.Ref.a.X", 3)])
        report = merge_fails(d1, d2)
        self.assertEqual(report.errors, ["commands: cannot rename 'Ref.a.X' from 'd2' to 'Q.Ref.a.X': that name is "
                                         "already defined in 'd1' with opcode 0x3"])
        d1 = make_dictionary("Ref.DeploymentA", commands=[command("Ref.a.X", 1)])
        d2 = make_dictionary("Ref.DeploymentB", commands=[command("Ref.a.X", 2)])
        d3 = make_dictionary("Ref.DeploymentC", commands=[command("DeploymentA.Ref.a.X", 3)])
        report = merge_fails(d1, d2, d3)
        self.assertEqual(count(report.errors, E10[1]), 1)
        self.assertEqual(count(report.warnings, W2), 1)
        # a third input whose bare name was renamed away earlier: unusable prefix, then target held by itself
        d3 = make_dictionary("Ref.bad name", commands=[command("Ref.a.X", 3)])
        report = merge_fails(d1, d2, d3)
        self.assertEqual(count(report.errors, E11[0]), 1)
        self.assertIn("'d3'", report.errors[0])
        d3 = make_dictionary("Ref.DeploymentC", commands=[command("Ref.a.X", 3), command("DeploymentC.Ref.a.X", 4)])
        report = merge_fails(d1, d2, d3)
        self.assertEqual(count(report.errors, E10[1]), 1)
        self.assertIn("'d3'", report.errors[0])

    def test_equal_prefix_via_contributor(self):
        d1 = make_dictionary("Ref.DA", commands=[command("Sub.X", 1)])
        d2 = make_dictionary("Ref.DB", commands=[command("Sub.X", 1)])
        d3 = make_dictionary("Other.DB", commands=[command("Sub.X", 2)])
        report = merge_fails(d1, d2, d3)
        self.assertEqual(count(report.errors, E15), 1)
        self.assertIn("inputs 'd2' and 'd3' both have deploymentName ending in 'DB'", report.errors[0])
        d3["metadata"]["deploymentName"] = "Other.DC"
        merged, report = merge_ok(d1, d2, d3)
        self.assertEqual([c["name"] for c in merged["commands"]], ["DA.Sub.X", "DC.Sub.X"])
        self.assertEqual(count(report.warnings, W2), 1)

    def test_prefix_validation(self):
        for bad, good in ((make_dictionary("Ref.bad name"), make_dictionary("Ref.Good")),
                          (make_dictionary("Ref.Good"), make_dictionary("Ref.bad name"))):
            bad["commands"] = [command("Sub.X", 1), command("Sub.Y", 2)]
            good["commands"] = [command("Sub.X", 3), command("Sub.Y", 4)]
            report = merge_fails(bad, good)
            self.assertEqual(count(report.errors, E11[0]), 1)
            self.assertEqual(len(report.errors), 1)  # once per input, not once per collision
            merge_ok(bad, good, no_namespace=True, prefer_primary=True)
        # no collision: never evaluated
        merge_ok(make_dictionary("Ref.bad name", commands=[command("Sub.X", 1)]),
                 make_dictionary("Ref.Good", commands=[command("Sub.Y", 2)]))
        missing = make_dictionary("Ref.One", commands=[command("Sub.X", 2)], deploymentName=None)
        report = merge_fails(make_dictionary("Ref.Two", commands=[command("Sub.X", 1)]), missing)
        self.assertEqual(count(report.errors, E11[1]), 1)

    def test_collect_all_reports_every_error(self):
        d1 = make_dictionary("Ref.One", commands=[command("Sub.X", 1), command("Ref.a.Y", 2)],
                             types=[alias_type("Ref.T", 32)], projectVersion="p1")
        d2 = make_dictionary("Ref.Two", commands=[command("Sub.X", 3), command("Ref.b.Y", 2)],
                             types=[alias_type("Ref.T", 16)], projectVersion="p2")
        existing = self.tmp / "out.json"
        existing.write_text("keep")
        code, lines, _ = self.run_cli("--no-namespace", self.write("d1.json", d1), self.write("d2.json", d2),
                                      output=existing)
        self.assertEqual(code, 1)
        self.assertEqual([count(lines, *f) for f in (E1, (E3,), (E4,), (E5,))], [1, 1, 1, 1])
        self.assertIn("Merge failed with 4 error(s) and 0 warning(s)", lines[-1])
        self.assertEqual(existing.read_text(), "keep")


class TestMetadata(DictionaryMergeTestCase):

    def test_metadata_versions(self):
        d1 = make_dictionary("Ref.One")
        for field_name in ("projectVersion", "frameworkVersion", "dictionarySpecVersion"):
            d2 = make_dictionary("Ref.Two", **{field_name: "other"})
            report = merge_fails(d1, d2)
            self.assertEqual(count(report.errors, E5), 1)
            self.assertIn(f"field '{field_name}'", report.errors[0])
            merged, _ = merge_ok(d1, d2, permissive=True)
            self.assertEqual(merged["metadata"][field_name], d1["metadata"][field_name])
        # both missing a field: equal; one missing: E5 printing None
        merge_ok(make_dictionary("Ref.One", projectVersion=None), make_dictionary("Ref.Two", projectVersion=None))
        report = merge_fails(d1, make_dictionary("Ref.Two", projectVersion=None))
        self.assertIn("(p1 vs None)", report.errors[0])

    def test_library_versions_warning(self):
        d1 = make_dictionary("Ref.One", libraryVersions=["lib@1"])
        d2 = make_dictionary("Ref.Two", libraryVersions=["lib@2"])
        merged, report = merge_ok(d1, d2)
        self.assertEqual(count(report.warnings, W6), 1)
        self.assertEqual(merged["metadata"]["libraryVersions"], ["lib@1"])
        _, report = merge_ok(d1, d2, permissive=True)
        self.assertEqual(report.warnings, [])
        _, report = merge_ok(d1, make_dictionary("Ref.Two", libraryVersions=None))
        self.assertEqual(report.warnings, [])

    def test_deployment_name_default_and_flag(self):
        merged, _ = merge_ok(make_dictionary("Ref.One"), make_dictionary("Ref.Two"))
        self.assertEqual(merged["metadata"]["deploymentName"], "Ref.One_Ref.Two_merged")
        merged, _ = merge_ok(make_dictionary("Ref.One"), make_dictionary("Ref.Two"), make_dictionary("Ref.Three"))
        self.assertEqual(merged["metadata"]["deploymentName"], "Ref.One_Ref.Two_Ref.Three_merged")
        d1, d2 = self.write("d1.json", make_dictionary("Ref.One")), self.write("d2.json", make_dictionary("Ref.Two"))
        code, _, output = self.run_cli("--name", "M.T", d1, d2)
        self.assertEqual((code, load(output)["metadata"]["deploymentName"]), (0, "M.T"))
        code, lines, _ = self.run_cli("--name", "1bad", d1, d2)
        self.assertEqual((code, lines), (1, ["[ERROR] --name '1bad' is an invalid identifier"]))

    def test_missing_metadata_errors(self):
        d1 = make_dictionary("Ref.One")
        for broken in ({}, [], "meta"):
            d2 = make_dictionary("Ref.Two")
            d2["metadata"] = broken
            if broken == {}:
                del d2["metadata"]
            report = merge_fails(d1, d2)
            self.assertEqual(count(report.errors, E7), 1)
        merge_ok(d1, make_dictionary("Ref.Two", frameworkVersion=None, libraryVersions=None), permissive=True)


class TestStructure(DictionaryMergeTestCase):

    def test_missing_section_errors(self):
        for section in dictionary_merge.SECTION_ORDER[1:]:
            d2 = make_dictionary("Ref.Two")
            del d2[section]
            report = merge_fails(make_dictionary("Ref.One"), d2)
            self.assertEqual(report.errors, [f"Malformed dictionary 'd2'. Missing key: '{section}'"])
            d2[section] = {}
            report = merge_fails(make_dictionary("Ref.One"), d2)
            self.assertEqual(report.errors, [f"Malformed dictionary 'd2'. Section '{section}' is not an array"])

    def test_missing_id_errors(self):
        cases = [("commands", {"commandKind": "async", "opcode": 1}, "name"),
                 ("commands", {"name": "Ref.a.X"}, "opcode"),
                 ("events", {"name": "Ref.a.E"}, "id"),
                 ("typeDefinitions", {"kind": "alias"}, "qualifiedName"),
                 ("telemetryPacketSets", {"members": []}, "name")]
        for section, entry, key in cases:
            d2 = make_dictionary("Ref.Two")
            d2[section] = [entry]
            report = merge_fails(make_dictionary("Ref.One"), d2)
            self.assertEqual(report.errors, [f"Malformed dictionary section '{section}' in 'd2'. Entry #0 missing "
                                             f"key: '{key}'"])
        d2 = make_dictionary("Ref.Two")
        d2["commands"] = [42]
        self.assertEqual(merge_fails(make_dictionary("Ref.One"), d2).errors,
                         ["Malformed dictionary section 'commands' in 'd2'. Entry #0 is not an object"])
        d2["commands"] = [{"name": 7, "opcode": 1}]
        self.assertEqual(merge_fails(make_dictionary("Ref.One"), d2).errors,
                         ["Malformed dictionary section 'commands' in 'd2'. Entry #0: 'name' must be a string (got 7)"])

    def test_packet_set_shape_errors(self):
        good = make_dictionary("Ref.One", channels=[channel("Ref.a.X", 1)])
        expected = ["Malformed dictionary section 'telemetryPacketSets' in 'd2'. Set 'Pkts' must have 'members' "
                    "packets with 'members' arrays of channel names and an 'omitted' array of channel names"]
        for bad_set in ({"name": "Pkts", "members": [{"name": "P", "id": 1}]},
                        {"name": "Pkts", "members": {}},
                        {"name": "Pkts", "members": [{"name": "P", "id": 1, "members": [["Ref.a.X"]]}]},
                        {"name": "Pkts", "members": [{"name": "P", "id": 1, "members": [{"n": 1}]}]},
                        {"name": "Pkts", "members": [], "omitted": [3]},
                        {"name": "Pkts", "members": [], "omitted": "Ref.a.X"}):
            d2 = make_dictionary("Ref.Two", channels=[channel("Ref.b.Y", 2)])
            d2["telemetryPacketSets"] = [bad_set]
            self.assertEqual(merge_fails(good, d2).errors, expected)
        # a set without 'members' is what the GDS loads as an empty set: accepted and emitted unchanged
        d2 = make_dictionary("Ref.Two", channels=[channel("Ref.a.X", 2)])
        d2["telemetryPacketSets"] = [{"name": "Pkts"}]
        merged, _ = merge_ok(good, d2)
        self.assertEqual(merged["telemetryPacketSets"], [{"name": "Pkts"}])

    def test_phase4_skipped_after_phase3_errors(self):
        d1 = make_dictionary("Ref.One", channels=[channel("Ref.a.X", 1)])
        d2 = make_dictionary("Ref.Two", channels=[channel("Ref.b.Y", 1)],
                             packet_sets=[packet_set("Pkts", [packet("P", 1, ["Ref.b.Y"])])])
        report = merge_fails(d1, d2)
        self.assertEqual(count(report.errors, E3), 1)
        self.assertEqual(len(report.errors), 1)

    def test_id_must_be_strict_int(self):
        for value in (True, 1.0, "1"):
            d2 = make_dictionary("Ref.Two", commands=[command("Ref.a.X", value)])
            report = merge_fails(make_dictionary("Ref.One"), d2)
            self.assertEqual(count(report.errors, "'opcode' must be an integer"), 1)
            self.assertIn(f"(got {json.dumps(value)})", report.errors[0])
        merge_ok(make_dictionary("Ref.One"), make_dictionary("Ref.Two", commands=[command("Ref.a.X", 0)]))

    def test_within_input_duplicates(self):
        d1 = make_dictionary("Ref.One", commands=[command("Ref.a.X", 1), command("Ref.a.X", 1)],
                             types=[alias_type("Ref.T", 32), alias_type("Ref.T", 32)])
        merged, _ = merge_ok(d1, make_dictionary("Ref.Two"))
        self.assertEqual(len(merged["commands"]), 1)
        self.assertEqual(len(merged["typeDefinitions"]), 1)
        d1["commands"][1]["annotation"] = "differs"
        d1["typeDefinitions"][1]["type"] = type_of("U16", 16)
        report = merge_fails(d1, make_dictionary("Ref.Two"))
        self.assertEqual(count(report.errors, E12), 2)
        self.assertIn("'Ref.a.X' is defined twice", report.errors[0])
        d1 = make_dictionary("Ref.One", commands=[command("Ref.a.X", 1), command("Ref.a.Y", 1)])
        report = merge_fails(d1, make_dictionary("Ref.Two"))
        self.assertIn("'opcode 0x1' is defined twice", report.errors[0])

    def test_not_json(self):
        good = self.write("good.json", make_dictionary("Ref.One"))
        code, lines, _ = self.run_cli(good, self.tmp / "missing.json")
        self.assertEqual((code, lines), (1, [f"[ERROR] '{self.tmp / 'missing.json'}' does not exist"]))
        bad = self.tmp / "bad.json"
        bad.write_text("{not json")
        code, lines, _ = self.run_cli(good, bad)
        self.assertEqual(code, 1)
        self.assertTrue(lines[0].startswith(f"[ERROR] '{bad}': "))
        array = self.tmp / "array.json"
        array.write_text("[]")
        code, lines, _ = self.run_cli(good, array)
        self.assertEqual((code, lines), (1, [f"[ERROR] '{array}' is not a JSON object"]))

    def test_usage_errors_exit_2(self):
        for argv in ([str(A_PATH)], ["--permissive", str(A_PATH)], []):
            with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as context:
                dictionary_merge.main(argv)
            self.assertEqual(context.exception.code, 2)

    def test_options_between_positionals(self):
        b2, c2 = self.write("B2.json", self.B2), self.write("C2.json", self.C2)
        code, _, output = self.run_cli(A_PATH, b2, "--permissive", c2)
        self.assertEqual(code, 0)
        merged = load(output)
        self.assertTrue(merged["metadata"]["deploymentName"].endswith(".DeploymentC_merged"))
        self.assertEqual(len(merged["commands"]), len(self.M["commands"]) + len(self.C2["commands"]))

    def test_merge_dictionaries_wrapper(self):
        merged = merge_dictionaries(self.A, load(GROUND_PATH), permissive=True)
        self.assertEqual(merged, load(GOLDEN_GROUND))
        with self.assertRaises(ValueError) as context:
            merge_dictionaries(self.A, self.B)
        self.assertEqual(len(str(context.exception).splitlines()), 85)
        self.assertEqual(self.A, load(A_PATH))


class TestPacketSets(DictionaryMergeTestCase):

    def two_with_packets(self):
        d1 = make_dictionary("Ref.DeploymentA", channels=[channel("Sub.c.X", 1), channel("Sub.c.Y", 2)],
                             packet_sets=[packet_set("Pkts", [packet("P", 1, ["Sub.c.X", "Sub.c.Y"])],
                                                     omitted=["Sub.c.X"])])
        d2 = make_dictionary("Ref.DeploymentB", channels=[channel("Sub.c.X", 3), channel("Ref.b.Z", 4)],
                             packet_sets=[packet_set("Pkts", [packet("Q", 1, ["Sub.c.X", "Ref.b.Z"])],
                                                     omitted=["Sub.c.X"])])
        return d1, d2

    def test_packet_members_rewritten_on_rename(self):
        d1, d2 = self.two_with_packets()
        d3 = make_dictionary("Ref.DeploymentC", channels=[channel("Ref.c.W", 5)],
                             packet_sets=[packet_set("Other", [packet("R", 1, ["Sub.c.X"])])])
        # d3 never contributed to the Sub.c.X slot, so its bare reference is not rewritten: E6 in phase 4
        report = merge_fails(d1, d2, d3)
        self.assertEqual(report.errors, ["telemetryPacketSets: packet 'Other/R' in 'd3' references unknown channel "
                                         "'Sub.c.X' in members"])
        d3["telemetryPacketSets"][0]["members"][0]["members"] = ["Ref.c.W"]
        merged, report = merge_ok(d1, d2, d3)
        sets = {s["name"]: s for s in merged["telemetryPacketSets"]}
        self.assertEqual(set(sets), {"DeploymentA.Pkts", "DeploymentB.Pkts", "Other"})
        self.assertEqual(sets["DeploymentA.Pkts"]["members"][0]["members"], ["DeploymentA.Sub.c.X", "Sub.c.Y"])
        self.assertEqual(sets["DeploymentA.Pkts"]["omitted"], ["DeploymentA.Sub.c.X"])
        self.assertEqual(sets["DeploymentB.Pkts"]["members"][0]["members"], ["DeploymentB.Sub.c.X", "Ref.b.Z"])
        self.assertEqual(sets["DeploymentB.Pkts"]["omitted"], ["DeploymentB.Sub.c.X"])
        self.assertEqual(count(report.warnings, W2), 2)
        self.assertEqual(count(report.warnings, W11), 1)
        self.assertIn("output holds 3 packet sets ('DeploymentA.Pkts', 'DeploymentB.Pkts', 'Other')",
                      report.warnings[-1])

    def test_packet_members_rewritten_in_main_after_late_collision(self):
        d1, _ = self.two_with_packets()
        d2 = make_dictionary("Ref.DeploymentB", channels=[channel("Ref.b.Z", 4)])
        d3 = make_dictionary("Ref.DeploymentC", channels=[channel("Sub.c.X", 9)])
        merged, _ = merge_ok(d1, d2, d3)
        self.assertEqual(merged["telemetryPacketSets"][0]["members"][0]["members"], ["DeploymentA.Sub.c.X", "Sub.c.Y"])
        self.assertEqual([c["name"] for c in merged["telemetryChannels"]],
                         ["DeploymentA.Sub.c.X", "Sub.c.Y", "Ref.b.Z", "DeploymentC.Sub.c.X"])

    def test_packet_members_removed_on_drop(self):
        d1 = make_dictionary("Ref.DeploymentA", channels=[channel("Sub.c.X", 1)])
        d2 = make_dictionary("Ref.DeploymentB", channels=[channel("Sub.c.X", 3), channel("Ref.b.Z", 4)],
                             packet_sets=[packet_set("Pkts", [packet("Q", 1, ["Sub.c.X", "Ref.b.Z"]),
                                                              packet("R", 2, ["Ref.b.Z"])], omitted=["Sub.c.X"])])
        merged, report = merge_ok(d1, d2, no_namespace=True, prefer_primary=True)
        packets = merged["telemetryPacketSets"][0]
        self.assertEqual([p["name"] for p in packets["members"]], ["R"])
        self.assertEqual(packets["members"][0]["members"], ["Ref.b.Z"])
        self.assertEqual(packets["omitted"], [])
        self.assertEqual([count(report.warnings, w) for w in (W1, W7, W8)], [1, 1, 1])
        # W4 (same id, different name) drops too
        d2["telemetryChannels"][0] = channel("Ref.b.Other", 1)
        d2["telemetryPacketSets"][0]["members"][0]["members"] = ["Ref.b.Other", "Ref.b.Z"]
        d2["telemetryPacketSets"][0]["omitted"] = ["Ref.b.Other"]
        merged, report = merge_ok(d1, d2, prefer_primary=True)
        self.assertEqual([p["name"] for p in merged["telemetryPacketSets"][0]["members"]], ["R"])
        self.assertEqual([count(report.warnings, w) for w in (W4, W7, W8)], [1, 1, 1])

    def test_packet_unknown_channel_errors(self):
        d1 = make_dictionary("Ref.DeploymentA", channels=[channel("Ref.a.X", 1)],
                             packet_sets=[packet_set("Pkts", [packet("P", 1, ["Ref.a.X", "Ref.a.Nope"])])])
        d2 = make_dictionary("Ref.DeploymentB", channels=[channel("Ref.b.Y", 2)],
                             packet_sets=[packet_set("Other", [packet("Q", 1, ["Ref.b.Nope"])])])
        report = merge_fails(d1, d2)
        self.assertEqual(count(report.errors, E6), 2)
        self.assertIn("packet 'Pkts/P' in 'd1' references unknown channel 'Ref.a.Nope' in members", report.errors[0])
        d1["telemetryPacketSets"][0]["members"][0]["members"] = ["Ref.a.X"]
        d1["telemetryPacketSets"][0]["omitted"] = ["Ref.a.Stale"]
        d2["telemetryPacketSets"] = []
        code, lines, output = self.run_cli(self.write("d1.json", d1), self.write("d2.json", d2))
        self.assertEqual(code, 0)
        self.assertEqual(count(lines, W10), 1)
        self.assertEqual(load(output)["telemetryPacketSets"][0]["omitted"], ["Ref.a.Stale"])
        globals_cleanup()
        self.addCleanup(globals_cleanup)
        _, ch_names, _ = ChJsonLoader(str(output)).construct_dicts(str(output))
        _, pkt_names, _ = PktJsonLoader(str(output)).construct_dicts("Pkts", ch_names)
        self.assertEqual(list(pkt_names), ["P"])

    def test_packet_set_dedupe_rename_prefer(self):
        d1 = make_dictionary("Ref.DeploymentA", channels=[channel("Ref.a.X", 1)],
                             packet_sets=[packet_set("Pkts", [packet("P", 1, ["Ref.a.X"])])])
        same = copy.deepcopy(d1)
        same["metadata"]["deploymentName"] = "Ref.DeploymentB"
        merged, report = merge_ok(d1, same)
        self.assertEqual((merged["telemetryPacketSets"], report.warnings), (d1["telemetryPacketSets"], []))
        differing = make_dictionary("Ref.DeploymentB", channels=[channel("Ref.b.Y", 2)],
                                    packet_sets=[packet_set("Pkts", [packet("Q", 1, ["Ref.b.Y"])])])
        merged, report = merge_ok(d1, differing)
        self.assertEqual([s["name"] for s in merged["telemetryPacketSets"]], ["DeploymentA.Pkts", "DeploymentB.Pkts"])
        self.assertEqual(count(report.warnings, W2), 1)
        self.assertEqual(count(report.warnings, W11), 1)
        self.assertNotIn("0x", report.warnings[0])
        report = merge_fails(d1, differing, no_namespace=True)
        self.assertEqual(count(report.errors, "with different definitions; use --prefer-primary"), 1)
        merged, report = merge_ok(d1, differing, no_namespace=True, prefer_primary=True)
        self.assertEqual(merged["telemetryPacketSets"], d1["telemetryPacketSets"])
        self.assertEqual(count(report.warnings, W1), 1)
        self.assertEqual(count(report.warnings, W11), 0)


class TestAtomicWrite(DictionaryMergeTestCase):

    def setUp(self):
        super().setUp()
        self.d1 = self.write("d1.json", make_dictionary("Ref.One", commands=[command("Ref.a.X", 1)]))
        self.d2 = self.write("d2.json", make_dictionary("Ref.Two", commands=[command("Ref.b.Y", 2)]))

    def test_atomic_write(self):
        for umask in (0o022, 0o077):
            previous = os.umask(umask)
            try:
                output = self.tmp / f"mode_{umask:o}.json"
                code, _, _ = self.run_cli(self.d1, self.d2, output=output)
            finally:
                os.umask(previous)
            self.assertEqual(code, 0)
            self.assertEqual(stat.S_IMODE(output.stat().st_mode), 0o666 & ~umask)
        self.assertEqual([p.name for p in self.tmp.glob("*.tmp")], [])
        # a symlink is replaced by a regular file; the target is untouched
        target = self.tmp / "target.json"
        target.write_text("target")
        link = self.tmp / "link.json"
        link.symlink_to(target)
        code, _, _ = self.run_cli(self.d1, self.d2, output=link)
        self.assertEqual(code, 0)
        self.assertFalse(link.is_symlink())
        self.assertEqual(target.read_text(), "target")
        # a failing replace leaves the old file and no temporary
        existing = self.tmp / "existing.json"
        existing.write_text("old")
        with mock.patch.object(dictionary_merge.os, "replace", side_effect=OSError("disk full")):
            code, lines, _ = self.run_cli(self.d1, self.d2, output=existing)
        self.assertEqual(code, 1)
        self.assertEqual(lines[-1], f"[ERROR] cannot write '{existing}': disk full")
        self.assertEqual(existing.read_text(), "old")
        self.assertEqual([p.name for p in self.tmp.glob("*.tmp")], [])
        # an existing file keeps its mode, whatever the umask
        existing.chmod(0o644)
        previous = os.umask(0o077)
        try:
            code, _, _ = self.run_cli(self.d1, self.d2, output=existing)
        finally:
            os.umask(previous)
        self.assertEqual(code, 0)
        self.assertEqual(stat.S_IMODE(existing.stat().st_mode), 0o644)
        # a directory that refuses the temporary file is an error, never a truncating direct write
        existing.write_text("old")
        with mock.patch.object(dictionary_merge.tempfile, "NamedTemporaryFile",
                               side_effect=PermissionError("read-only directory")):
            code, lines, _ = self.run_cli(self.d1, self.d2, output=existing)
        self.assertEqual(code, 1)
        self.assertEqual(lines[-1], f"[ERROR] cannot write '{existing}': read-only directory")
        self.assertEqual(existing.read_text(), "old")

    def test_unwritable_directory(self):
        output = self.tmp / "nope" / "out.json"
        code, lines, _ = self.run_cli(self.d1, self.d2, output=output)
        self.assertEqual(code, 1)
        self.assertTrue(lines[-1].startswith(f"[ERROR] cannot write '{output}': "))
        self.assertFalse(output.parent.exists())

    @unittest.skipUnless(hasattr(os, "mkfifo"), "FIFOs are POSIX-only")
    def test_fifo_output_uses_direct_write(self):
        fifo = self.tmp / "out.fifo"
        os.mkfifo(fifo)
        received = []

        def reader():
            with open(fifo, "r") as file_handle:
                received.append(file_handle.read())

        thread = threading.Thread(target=reader, daemon=True)
        thread.start()
        code, _, _ = self.run_cli(self.d1, self.d2, output=fifo)
        thread.join(timeout=10)
        self.assertFalse(thread.is_alive(), "the tool never opened the FIFO for writing")
        self.assertEqual(code, 0)
        code, _, regular = self.run_cli(self.d1, self.d2)
        self.assertEqual(received, [regular.read_text()])

    @unittest.skipUnless(Path("/dev/stdout").exists(), "/dev/stdout is POSIX-only")
    def test_dev_stdout_redirected_to_file_uses_direct_write(self):
        _, _, regular = self.run_cli(self.d1, self.d2)
        for descriptor in (Path("/dev/stdout"), Path("/dev/fd/1"), Path("/proc/self/fd/1")):
            if not descriptor.exists():
                continue
            redirected = self.tmp / "redirected.json"
            saved = os.dup(1)
            try:
                with open(redirected, "w") as file_handle:
                    os.dup2(file_handle.fileno(), 1)
                self.assertTrue(descriptor.is_file())
                code, _, _ = self.run_cli(self.d1, self.d2, output=descriptor)
            finally:
                os.dup2(saved, 1)
                os.close(saved)
            self.assertEqual(code, 0, descriptor)
            self.assertEqual(redirected.read_text(), regular.read_text(), descriptor)

    def test_stream_output_detection(self):
        regular = self.tmp / "plain.json"
        regular.write_text("{}")
        self.assertFalse(dictionary_merge.is_stream_output(regular))
        self.assertFalse(dictionary_merge.is_stream_output(self.tmp / "new.json"))
        link = self.tmp / "link.json"
        link.symlink_to(regular)
        self.assertFalse(dictionary_merge.is_stream_output(link))
        descriptor = self.tmp / "fd.json"
        descriptor.symlink_to("/proc/self/fd/1")
        self.assertTrue(dictionary_merge.is_stream_output(descriptor))
        self.assertTrue(dictionary_merge.is_stream_output(Path("/proc/self/fd/1")))
        if Path("/dev/fd").is_dir():
            self.assertTrue(dictionary_merge.is_stream_output(Path("/dev/fd/1")))
        if Path("/dev/shm").is_dir() and os.access("/dev/shm", os.W_OK):
            shm = Path("/dev/shm") / f"fprime_merge_test_{os.getpid()}.json"
            shm.write_text("{}")
            self.addCleanup(shm.unlink)
            self.assertFalse(dictionary_merge.is_stream_output(shm))
        self.assertEqual([p.name for p in self.tmp.glob("*.tmp")], [])
