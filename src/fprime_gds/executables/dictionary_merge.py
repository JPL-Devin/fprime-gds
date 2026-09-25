""" fprime_gds.executables.dictionary_merge: merge two or more F Prime JSON dictionaries

The first dictionary is primary (list the deployment the GDS is attached to first): its entries come first in every
section and it decides metadata and unknown top-level keys. With --prefer-primary a conflict is resolved in favour of
the earliest input holding the id or definition. Merging runs in phases; a phase that collected an error ends the run:

    0. argument parsing (usage errors exit 2, an invalid --name exits 1)
    1. load every input (fail-fast: unreadable / not JSON / not an object)
    2. structural validation of every input (all errors collected)
    3a. metadata, typeDefinitions, constants
    3b. commands, parameters, events, telemetryChannels, records, containers
    3c. telemetryPacketSets (after 3b so channel renames caused by any input are applied to every packet set)
    4. packet-set channel references (skipped when phase 3 collected errors: a rejected channel is not "unknown")
    5. atomic write of the output (only when no error was collected)

Within a unique section an arriving entry is classified in this order (see `classify_unique_entry`):

    1. identical to a held entry (same name, same id, same body)      -> merged into that entry, silently
    2. same id as a held entry                                          -> error (--prefer-primary keeps the earliest)
    3. same name as a held entry, different id                          -> both renamed '<prefix>.<name>' (default),
                                                                           or error / drop with --no-namespace
    4. bare name already renamed away earlier in this run               -> appended as '<prefix>.<name>'
    5. otherwise                                                        -> appended unchanged

`prefix` is the last dot-separated segment of the input's metadata.deploymentName (`LoadedInput.prefix`). Types and
constants are never renamed; a differing definition is an error unless --prefer-primary keeps the earliest one.
Packet sets of an input whose channels were renamed are rewritten; a packet referencing a channel dropped by
--prefer-primary is removed whole. Exit codes: 0 success (warnings possible), 1 any merge / input / output error,
2 usage error.
"""

import argparse
import copy
import json
import os
import re
import stat
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Dict, List, Optional, Set, Tuple

IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z_0-9]*(\.[A-Za-z_][A-Za-z_0-9]*)*")
SEGMENT_RE = re.compile(r"[A-Za-z_][A-Za-z_0-9]*")

UNIQUE_SECTIONS = {
    "commands": "opcode",
    "parameters": "id",
    "events": "id",
    "telemetryChannels": "id",
    "records": "id",
    "containers": "id",
}
NON_UNIQUE_SECTIONS = ("typeDefinitions", "constants")
PACKET_SECTION = "telemetryPacketSets"
SECTION_ORDER = [
    "metadata",
    "typeDefinitions",
    "constants",
    "commands",
    "parameters",
    "events",
    "telemetryChannels",
    "records",
    "containers",
    "telemetryPacketSets",
]
VERSION_FIELDS = ("projectVersion", "frameworkVersion", "dictionarySpecVersion")

ERROR = "ERROR"
WARNING = "WARNING"


@dataclass
class MergeOptions:
    """ Command line options that influence the merge """
    name: Optional[str] = None
    permissive: bool = False
    prefer_primary: bool = False
    no_namespace: bool = False


@dataclass
class MergeReport:
    """ Collect-all sink for errors and warnings, kept in generation order. Each warning carries a kind (renamed,
    dropped, overridden, ...) so a run that emits hundreds of routine lines can still be summarised per kind. """
    errors: List[str] = field(default_factory=list)
    warnings: List[str] = field(default_factory=list)
    kinds: Dict[str, int] = field(default_factory=dict)
    lines: List[Tuple[str, str]] = field(default_factory=list)

    def error(self, message):
        self.errors.append(message)
        self.lines.append((ERROR, message))

    def warning(self, kind, message):
        self.warnings.append(message)
        self.kinds[kind] = self.kinds.get(kind, 0) + 1
        self.lines.append((WARNING, message))

    def summary(self):
        """ One line of per-kind warning counts, e.g. '262 warning(s): 260 renamed, 2 dropped' """
        by_kind = ", ".join(f"{count} {kind}" for kind, count in sorted(self.kinds.items(), key=lambda k: -k[1]))
        return f"{len(self.warnings)} warning(s): {by_kind}"

    def print(self, stream=None):
        stream = sys.stderr if stream is None else stream
        for level, message in self.lines:
            print(f"[{level}] {message}", file=stream)


@dataclass
class LoadedInput:
    """ One input dictionary: 1-based position on the command line, display path, parsed content """
    index: int
    path: str
    data: dict
    sections: Dict[str, list] = field(default_factory=dict)

    def deployment_name(self):
        metadata = self.data.get("metadata")
        return metadata.get("deploymentName") if isinstance(metadata, dict) else None

    def prefix(self) -> Optional[str]:
        """ Namespace prefix of this input: the last dot-separated segment of metadata.deploymentName """
        name = self.deployment_name()
        return name.rsplit(".", 1)[-1] if isinstance(name, str) else None


@dataclass
class Slot:
    """ One held entry of a section: the entry, the input it arrived from, its arrival name, and every input merged
    into it (input index -> the name that input's entry arrived with) """
    entry: dict
    origin: int
    orig_name: str
    contributors: Dict[int, str]


def strict_suffixes(name):
    """ Every strict dotted suffix of a name: 'a.b.c' -> ['b.c', 'c'] """
    parts = name.split(".")
    return [".".join(parts[i:]) for i in range(1, len(parts))]


def renamed(entry, new_name):
    """ Shallow copy of an entry with a new 'name'; key order (hence output byte order) is preserved """
    return {key: (new_name if key == "name" else value) for key, value in entry.items()}


def is_strict_int(value):
    return isinstance(value, int) and not isinstance(value, bool)


def format_id(id_key, value):
    return f"{id_key} 0x{value:X}"


class SectionAccumulator:
    """ Merge state of one section: held slots plus the indices needed to classify an arriving entry """

    def __init__(self, section, id_key):
        self.section = section
        self.id_key = id_key
        self.slots: List[Slot] = []
        self.by_name: Dict[str, int] = {}
        self.by_id: Dict[int, int] = {}
        self.by_orig: Dict[str, List[int]] = {}
        self.by_tail: Dict[str, List[int]] = {}

    def append(self, entry, origin, arrival_name):
        """ The only way a slot enters the accumulator; registers every index """
        idx = len(self.slots)
        self.slots.append(Slot(entry, origin, arrival_name, {origin: arrival_name}))
        self.by_name[entry["name"]] = idx
        if self.id_key is not None:
            self.by_id[entry[self.id_key]] = idx
        self.by_orig.setdefault(arrival_name, []).append(idx)
        for suffix in strict_suffixes(entry["name"]):
            self.by_tail.setdefault(suffix, []).append(idx)
        return idx

    def attach(self, idx, k, arrival_name, rename_map):
        """ Record that input k's entry named arrival_name merged into slot idx """
        slot = self.slots[idx]
        slot.contributors[k] = arrival_name
        if slot.entry["name"] != arrival_name:
            rename_map[k][self.section][arrival_name] = slot.entry["name"]

    def rename_slot(self, idx, new_name, rename_map):
        """ Rename a held slot in place and propagate the new name to every contributor's rename map """
        slot = self.slots[idx]
        old_name = slot.entry["name"]
        del self.by_name[old_name]
        for suffix in strict_suffixes(old_name):
            self.by_tail[suffix].remove(idx)
            if not self.by_tail[suffix]:
                del self.by_tail[suffix]
        slot.entry["name"] = new_name
        self.by_name[new_name] = idx
        for suffix in strict_suffixes(new_name):
            self.by_tail.setdefault(suffix, []).append(idx)
        for j, arrival_name in slot.contributors.items():
            rename_map[j][self.section][arrival_name] = new_name

    def held_prefixes(self, idxs, inputs):
        """ (prefix, contributor index) of every contributor of the given slots that has a usable prefix """
        result = []
        for idx in idxs:
            for j in self.slots[idx].contributors:
                prefix = inputs[j - 1].prefix()
                if prefix is not None:
                    result.append((prefix, j))
        return result

    def entries(self):
        return [slot.entry for slot in self.slots]


def load_input(index, path) -> LoadedInput:
    """ Read and parse one input (fail-fast) """
    try:
        with open(path, "r") as file_handle:
            data = json.load(file_handle)
    except (OSError, ValueError) as error:
        raise ValueError(f"'{path}': {error}")
    if not isinstance(data, dict):
        raise ValueError(f"'{path}' is not a JSON object")
    return LoadedInput(index, str(path), data)


def _collapse_duplicates(inp, section, key_of, describe, report):
    """ Collapse identical within-input duplicates to the first occurrence; differing duplicates are an error """
    seen = {}
    kept = []
    for entry in inp.sections[section]:
        keys = key_of(entry)
        differing = [key for key in keys if key in seen and seen[key] != entry]
        if differing:
            report.error(f"Malformed dictionary section '{section}' in '{inp.path}'. '{describe(differing[0])}' is "
                         f"defined twice with different definitions")
        elif not any(key in seen for key in keys):
            kept.append(entry)
        for key in keys:
            seen.setdefault(key, entry)
    inp.sections[section] = kept


def validate_structure(inp: LoadedInput, report: MergeReport):
    """ Phase 2: ten sections present with the right JSON types, entries carry their keys, ids are integers,
    within-input duplicates are identical. Fills `inp.sections` with the (collapsed) section lists. """
    data = inp.data
    malformed = f"Malformed dictionary '{inp.path}'."
    if "metadata" not in data:
        report.error(f"{malformed} Missing key: 'metadata'")
    elif not isinstance(data["metadata"], dict):
        report.error(f"{malformed} 'metadata' is not an object")

    for section in SECTION_ORDER[1:]:
        if section not in data:
            report.error(f"{malformed} Missing key: '{section}'")
            continue
        if not isinstance(data[section], list):
            report.error(f"{malformed} Section '{section}' is not an array")
            continue
        inp.sections[section] = list(data[section])

    def check_entries(section, required, id_key=None):
        """ Drop entries missing required keys (reported), return True when the section is usable """
        if section not in inp.sections:
            return False
        where = f"Malformed dictionary section '{section}' in '{inp.path}'."
        usable = []
        for position, entry in enumerate(inp.sections[section]):
            if not isinstance(entry, dict):
                report.error(f"{where} Entry #{position} is not an object")
                continue
            missing = [key for key in required if key not in entry]
            if missing:
                report.error(f"{where} Entry #{position} missing key: '{missing[0]}'")
                continue
            not_text = [key for key in required if key != id_key and not isinstance(entry[key], str)]
            if not_text:
                report.error(f"{where} Entry #{position}: '{not_text[0]}' must be a string "
                             f"(got {json.dumps(entry[not_text[0]])})")
                continue
            if id_key is not None and not is_strict_int(entry[id_key]):
                report.error(f"{where} Entry '{entry['name']}': '{id_key}' must be an integer "
                             f"(got {json.dumps(entry[id_key])})")
                continue
            usable.append(entry)
        inp.sections[section] = usable
        return True

    for section, id_key in UNIQUE_SECTIONS.items():
        if check_entries(section, ["name", id_key], id_key):
            _collapse_duplicates(inp, section, lambda e, k=id_key: [("name", e["name"]), (k, e[k])],
                                 lambda key, k=id_key: key[1] if key[0] == "name" else format_id(k, key[1]), report)
    for section in NON_UNIQUE_SECTIONS:
        if check_entries(section, ["qualifiedName"]):
            _collapse_duplicates(inp, section, lambda e: [e["qualifiedName"]], str, report)
    if check_entries(PACKET_SECTION, ["name"]):
        where = f"Malformed dictionary section '{PACKET_SECTION}' in '{inp.path}'."
        usable = []
        for packet_set in inp.sections[PACKET_SECTION]:
            packets = packet_set.get("members", [])
            omitted = packet_set.get("omitted", [])
            ok = (
                isinstance(packets, list)
                and isinstance(omitted, list)
                and all(isinstance(member, str) for member in omitted)
                and all(
                    isinstance(packet, dict)
                    and isinstance(packet.get("members"), list)
                    and all(isinstance(member, str) for member in packet["members"])
                    for packet in packets
                )
            )
            if not ok:
                report.error(f"{where} Set '{packet_set['name']}' must have 'members' packets with 'members' arrays "
                             f"of channel names and an 'omitted' array of channel names")
                continue
            usable.append(packet_set)
        inp.sections[PACKET_SECTION] = usable
        _collapse_duplicates(inp, PACKET_SECTION, lambda e: [e["name"]], str, report)


def metadata_inconsistencies(metadata1, metadata2):
    """ Version fields that differ between two metadata blocks, one message per field (a missing field reads None) """
    messages = []
    for field_name in VERSION_FIELDS:
        value1 = metadata1.get(field_name)
        value2 = metadata2.get(field_name)
        if value1 != value2:
            messages.append(f"Inconsistent metadata values for field '{field_name}'. ({value1} vs {value2}); use "
                            f"--permissive to ignore")
    return messages


def merge_metadata(inputs: List[LoadedInput], opts: MergeOptions, report: MergeReport):
    """ Merge the metadata blocks of all inputs, preferring the primary when there is a discrepancy """
    primary = inputs[0].data["metadata"]
    for inp in inputs[1:]:
        metadata = inp.data["metadata"]
        if not opts.permissive:
            for message in metadata_inconsistencies(primary, metadata):
                report.error(f"metadata: {message}")
            if ("libraryVersions" in primary and "libraryVersions" in metadata
                    and primary["libraryVersions"] != metadata["libraryVersions"]):
                report.warning("metadata", f"metadata: libraryVersions differ between '{inputs[0].path}' and "
                                           f"'{inp.path}'; kept '{inputs[0].path}'")
    name = opts.name
    if name is None:
        name = "_".join(str(inp.data["metadata"].get("deploymentName", "unknown")) for inp in inputs) + "_merged"
    return {**primary, "deploymentName": name}


def merge_non_unique_section(held, inp: LoadedInput, section, opts: MergeOptions, report: MergeReport, inputs):
    """ typeDefinitions / constants: keyed by qualifiedName, identical definitions collapse, differing ones are an
    error unless --prefer-primary keeps the earliest input's definition """
    for entry in inp.sections[section]:
        qualified_name = entry["qualifiedName"]
        previous = held.get(qualified_name)
        if previous is None:
            held[qualified_name] = (entry, inp.index)
        elif previous[0] != entry:
            earliest = inputs[previous[1] - 1].path
            if opts.prefer_primary:
                report.warning("overridden", f"{section}: '{qualified_name}' differs in '{inp.path}'; kept "
                                             f"definition from '{earliest}'")
            else:
                report.error(f"{section}: '{qualified_name}' has inconsistent definitions in '{earliest}' and "
                             f"'{inp.path}'; use --prefer-primary to keep the earliest")


class Merger:
    """ State of one N-way merge """

    def __init__(self, inputs: List[LoadedInput], opts: MergeOptions, report: MergeReport):
        self.inputs = inputs
        self.opts = opts
        self.report = report
        self.rename_map: Dict[int, Dict[str, Dict[str, str]]] = {
            inp.index: {section: {} for section in list(UNIQUE_SECTIONS) + [PACKET_SECTION]} for inp in inputs}
        self.dropped: Dict[int, Dict[str, Set[str]]] = {
            inp.index: {section: set() for section in UNIQUE_SECTIONS} for inp in inputs}
        self.prefix_errors: Set[int] = set()

    def path(self, index):
        return self.inputs[index - 1].path

    def prefix(self, index):
        return self.inputs[index - 1].prefix()

    def valid_prefix(self, index, name):
        """ E11: validate input `index`'s prefix, reporting once per input; returns the prefix or None """
        prefix = self.prefix(index)
        if prefix is not None and SEGMENT_RE.fullmatch(prefix):
            return prefix
        if index not in self.prefix_errors:
            self.prefix_errors.add(index)
            if prefix is None:
                self.report.error(f"'{self.path(index)}': metadata.deploymentName is missing or not a string; a "
                                  f"namespace prefix is needed to rename '{name}'; add it or pass --no-namespace")
            else:
                self.report.error(f"'{self.path(index)}': the last segment of metadata.deploymentName "
                                  f"'{self.inputs[index - 1].deployment_name()}' is not an identifier and cannot be "
                                  f"used as a namespace prefix; fix it or pass --no-namespace")
        return None

    def equal_prefix_error(self, acc, idxs, k, name, id_text, prefix_k):
        """ E15 when input k's prefix equals that of any contributor of the given slots """
        for prefix, j in acc.held_prefixes(idxs, self.inputs):
            if prefix == prefix_k:
                self.report.error(f"{acc.section}: '{name}' is defined in '{self.path(k)}' with a different "
                                  f"{id_text}, but inputs '{self.path(j)}' and '{self.path(k)}' both have "
                                  f"deploymentName ending in '{prefix}' and would share the prefix '{prefix}.'; give "
                                  f"the deployments distinct names or pass --no-namespace")
                return True
        return False

    def target_held_error(self, acc, name, origin, target):
        """ E10 (first wording) when a rename target is already held """
        other = acc.slots[acc.by_name[target]]
        with_id = f" with {format_id(acc.id_key, other.entry[acc.id_key])}" if acc.id_key else ""
        self.report.error(f"{acc.section}: cannot rename '{name}' from '{self.path(origin)}' to '{target}': that "
                          f"name is already defined in '{self.path(other.origin)}'{with_id}")

    def suffix_warning(self, acc, entry, k):
        """ W9: an appended bare name is a strict suffix of a held name, or a held name is a strict suffix of it """
        name = entry["name"]
        tail = "if these are instances of one subtopology, re-run the merge N-way on the original dictionaries"
        if name in acc.by_tail:
            held = acc.slots[min(acc.by_tail[name])].entry["name"]
            self.report.warning("un-prefixed", f"{acc.section}: '{name}' from '{self.path(k)}' is appended "
                                               f"un-prefixed although '{held}' exists; {tail}")
            return
        matches = [acc.by_name[suffix] for suffix in strict_suffixes(name) if suffix in acc.by_name]
        if matches:
            held = acc.slots[min(matches)].entry["name"]
            self.report.warning("un-prefixed", f"{acc.section}: '{name}' from '{self.path(k)}' is appended although "
                                               f"un-prefixed '{held}' exists; {tail}")

    def feed(self, acc: SectionAccumulator, inp: LoadedInput, entry):
        """ Add one entry of an input to its accumulator. The primary seeds the accumulator as-is (nothing is held
        yet, so it is never classified and never W9-checked); every later input is classified. """
        if inp.index == 1:
            acc.append(dict(entry), 1, entry["name"])
        else:
            self.classify_unique_entry(acc, inp.index, entry)

    def classify_unique_entry(self, acc: SectionAccumulator, k, entry):
        """ Classify one entry of input k (k >= 2) against the accumulator; see the module docstring """
        opts, report, section, id_key = self.opts, self.report, acc.section, acc.id_key
        n = entry["name"]
        i = entry[id_key] if id_key else None
        id_text = format_id(id_key, i) if id_key else ""
        prefix_k = self.prefix(k)
        t = f"{prefix_k}.{n}" if prefix_k is not None else None
        rename_map = self.rename_map

        # 1. identical entry already held (bare, under k's own prefix, or renamed retroactively)
        if n in acc.by_name and acc.slots[acc.by_name[n]].entry == entry:
            acc.attach(acc.by_name[n], k, n, rename_map)
            return
        if not opts.no_namespace:
            if t is not None and t in acc.by_name and acc.slots[acc.by_name[t]].entry == renamed(entry, t):
                acc.attach(acc.by_name[t], k, n, rename_map)
                return
            for idx in acc.by_orig.get(n, []):
                if acc.slots[idx].entry == renamed(entry, acc.slots[idx].entry["name"]):
                    acc.attach(idx, k, n, rename_map)
                    return

        # 2. id already held
        if id_key and i in acc.by_id:
            h_idx = acc.by_id[i]
            held = acc.slots[h_idx]
            if held.orig_name == n or (not opts.no_namespace and held.entry["name"] == t):
                if opts.prefer_primary:
                    report.warning("overridden", f"{section}: '{n}' ({id_text}) differs in '{self.path(k)}'; kept "
                                                 f"definition '{held.entry['name']}' from '{self.path(held.origin)}' "
                                                 f"(packet references in '{self.path(k)}' follow it)")
                    acc.attach(h_idx, k, n, rename_map)
                else:
                    report.error(f"{section}: '{n}' ({id_text}) has different definitions in "
                                 f"'{self.path(held.origin)}' and '{self.path(k)}'; use --prefer-primary to keep the "
                                 f"first")
            elif opts.prefer_primary:
                report.warning("dropped", f"{section}: {id_text} — '{n}' from '{self.path(k)}' dropped in favour of "
                               f"'{held.entry['name']}' from '{self.path(held.origin)}'")
                self.dropped[k][section].add(n)
            else:
                report.error(f"{section}: {id_text} is used by '{held.entry['name']}' in '{self.path(held.origin)}' "
                             f"and '{n}' in '{self.path(k)}'; use --prefer-primary to keep the first (names cannot "
                             f"resolve an id clash)")
            return

        # 3. same name, different id
        if n in acc.by_name:
            h_idx = acc.by_name[n]
            held = acc.slots[h_idx]
            held_id = f" ({format_id(id_key, held.entry[id_key])})" if id_key else ""
            arriving_id = f" ({id_text})" if id_key else ""
            if opts.no_namespace:
                if opts.prefer_primary:
                    report.warning("dropped", f"{section}: '{n}'{arriving_id} from '{self.path(k)}' dropped in "
                                              f"favour of '{n}'{held_id} from '{self.path(held.origin)}'")
                    if section in self.dropped[k]:
                        self.dropped[k][section].add(n)
                else:
                    clause = f" with different {id_key}s" if id_key else " with different definitions"
                    report.error(f"{section}: '{n}' is defined in '{self.path(held.origin)}'{held_id} and "
                                 f"'{self.path(k)}'{arriving_id}{clause}; use --prefer-primary to keep the first, or "
                                 f"drop --no-namespace to keep both under '<prefix>.'-qualified names")
                return
            if held.entry["name"] != held.orig_name:
                report.error(f"{section}: '{n}'{arriving_id} from '{self.path(k)}' collides with the "
                             f"already-namespaced entry '{n}'{held_id} from '{self.path(held.origin)}'; rename it in "
                             f"'{self.path(k)}' or pass --no-namespace")
                return
            prefix_h = self.valid_prefix(held.origin, n)
            prefix_k = self.valid_prefix(k, n)
            if prefix_h is None or prefix_k is None:
                return
            if self.equal_prefix_error(acc, [h_idx], k, n, id_key or "definition", prefix_k):
                return
            target_h = f"{prefix_h}.{held.orig_name}"
            if target_h in acc.by_name:
                self.target_held_error(acc, held.orig_name, held.origin, target_h)
                return
            if t in acc.by_name:
                self.target_held_error(acc, n, k, t)
                return
            acc.rename_slot(h_idx, target_h, rename_map)
            acc.append(renamed(entry, t), k, n)
            rename_map[k][section][n] = t
            if id_key:
                report.warning("renamed", f"{section}: '{n}' has {format_id(id_key, held.entry[id_key])} in "
                               f"'{self.path(held.origin)}' and {id_text} in '{self.path(k)}'; renamed to "
                               f"'{target_h}' and '{t}' (pass --no-namespace to make this an error)")
            else:
                report.warning("renamed", f"{section}: '{n}' differs between '{self.path(held.origin)}' and "
                                          f"'{self.path(k)}'; renamed to '{target_h}' and '{t}' (pass --no-namespace "
                                          f"to make this an error)")
            return

        # 4. bare name already renamed away earlier in this run
        if not opts.no_namespace and n in acc.by_orig:
            idxs = acc.by_orig[n]
            prefix_k = self.valid_prefix(k, n)
            if prefix_k is None:
                return
            if self.equal_prefix_error(acc, idxs, k, n, id_key or "definition", prefix_k):
                return
            if t in acc.by_name:
                self.target_held_error(acc, n, k, t)
                return
            existing = ", ".join(f"'{acc.slots[idx].entry['name']}'" for idx in idxs)
            acc.append(renamed(entry, t), k, n)
            rename_map[k][section][n] = t
            arriving_id = f" ({id_text})" if id_key else ""
            report.warning("renamed", f"{section}: '{n}'{arriving_id} from '{self.path(k)}' renamed to '{t}' (name "
                                      f"already namespaced as {existing})")
            return

        # 5. new name and new id
        acc.append(dict(entry), k, n)
        self.suffix_warning(acc, entry, k)

    def merge_unique_sections(self):
        accumulators = {section: SectionAccumulator(section, id_key) for section, id_key in UNIQUE_SECTIONS.items()}
        for inp in self.inputs:
            for section in UNIQUE_SECTIONS:
                acc = accumulators[section]
                for entry in inp.sections[section]:
                    self.feed(acc, inp, entry)
        return accumulators

    def rewrite_packet_sets(self, inp: LoadedInput):
        """ Phase 3c, per input: apply the input's channel renames and drops to a deep copy of its packet sets """
        channel_renames = self.rename_map[inp.index]["telemetryChannels"]
        dropped = self.dropped[inp.index]["telemetryChannels"]
        result = []
        for packet_set in copy.deepcopy(inp.sections[PACKET_SECTION]):
            set_name = packet_set["name"]
            kept_packets = []
            for packet in packet_set.get("members", []):
                dropped_members = [member for member in packet["members"] if member in dropped]
                if dropped_members:
                    self.report.warning("packets removed", f"{PACKET_SECTION}: packet '{set_name}/"
                                                           f"{packet.get('name')}' from '{inp.path}' removed because "
                                                           f"it references dropped channel '{dropped_members[0]}'")
                    continue
                packet["members"] = [channel_renames.get(member, member) for member in packet["members"]]
                kept_packets.append(packet)
            if "members" in packet_set:
                packet_set["members"] = kept_packets
            if "omitted" in packet_set:
                kept_omitted = []
                for member in packet_set["omitted"]:
                    if member in dropped:
                        self.report.warning("packets removed", f"{PACKET_SECTION}: dropped channel '{member}' removed "
                                                               f"from omitted list of set '{set_name}' from "
                                                               f"'{inp.path}'")
                    else:
                        kept_omitted.append(channel_renames.get(member, member))
                packet_set["omitted"] = kept_omitted
            result.append(packet_set)
        return result

    def merge_packet_sets(self):
        acc = SectionAccumulator(PACKET_SECTION, None)
        for inp in self.inputs:
            for packet_set in self.rewrite_packet_sets(inp):
                self.feed(acc, inp, packet_set)
        return acc

    def validate_packet_references(self, packet_acc, channel_acc):
        """ Phase 4: every packet member must name a merged channel; unknown omitted names are only reported. The GDS
        loads a single packet set, so an output holding several gets a reminder to pick one with --packet-set-name """
        channel_names = channel_acc.by_name
        for slot in packet_acc.slots:
            packet_set = slot.entry
            path = self.path(slot.origin)
            for packet in packet_set.get("members", []):
                for member in packet["members"]:
                    if member not in channel_names:
                        self.report.error(f"{PACKET_SECTION}: packet '{packet_set['name']}/{packet.get('name')}' in "
                                          f"'{path}' references unknown channel '{member}' in members")
            for member in packet_set.get("omitted", []):
                if member not in channel_names:
                    self.report.warning("unknown omitted", f"{PACKET_SECTION}: set '{packet_set['name']}' in '{path}' "
                                                           f"lists unknown channel '{member}' in omitted (kept; the "
                                                           f"GDS ignores omitted)")
        if len(packet_acc.slots) > 1:
            names = ", ".join(f"'{slot.entry['name']}'" for slot in packet_acc.slots)
            self.report.warning("packet sets", f"{PACKET_SECTION}: output holds {len(packet_acc.slots)} packet sets "
                                               f"({names}); the GDS decodes one, select it with --packet-set-name")


def merge_all(inputs: List[LoadedInput], opts: MergeOptions) -> Tuple[Optional[dict], MergeReport, Merger]:
    """ Merge loaded inputs. Returns (merged dictionary or None when errors were collected, report, merger state) """
    report = MergeReport()
    merger = Merger(inputs, opts, report)
    for inp in inputs:
        validate_structure(inp, report)
    if report.errors:
        return None, report, merger

    metadata = merge_metadata(inputs, opts, report)
    non_unique = {section: {} for section in NON_UNIQUE_SECTIONS}
    for inp in inputs:
        for section in NON_UNIQUE_SECTIONS:
            merge_non_unique_section(non_unique[section], inp, section, opts, report, inputs)
    accumulators = merger.merge_unique_sections()
    packet_acc = merger.merge_packet_sets()
    if report.errors:
        return None, report, merger
    merger.validate_packet_references(packet_acc, accumulators["telemetryChannels"])
    if report.errors:
        return None, report, merger

    merged = {}
    for inp in reversed(inputs):
        merged.update(inp.data)
    merged["metadata"] = metadata
    for section in NON_UNIQUE_SECTIONS:
        merged[section] = [entry for entry, _ in non_unique[section].values()]
    for section in UNIQUE_SECTIONS:
        merged[section] = accumulators[section].entries()
    merged[PACKET_SECTION] = packet_acc.entries()
    return merged, report, merger


def merge_dictionaries(dictionary1, dictionary2, name=None, permissive=False):
    """ Merge two dictionaries

    Thin wrapper over `merge_all` for two already-parsed dictionaries. Unknown fields are preserved preferring
    dictionary1's content. Default options apply (same-name/different-id entries are renamed, id conflicts raise).

    Args:
        dictionary1: dictionary 1's content
        dictionary2: dictionary 2's content
        name: new 'deploymentName' field
        permissive: allow miss-matched dictionary versions

    Return: merged dictionaries
    Throws:
        ValueError listing every collected error
    """
    inputs = [LoadedInput(1, "dictionary1", dictionary1), LoadedInput(2, "dictionary2", dictionary2)]
    merged, report, _ = merge_all(inputs, MergeOptions(name=name, permissive=permissive))
    if merged is None:
        raise ValueError("\n".join(report.errors))
    return merged


def write_output(path: Path, merged):
    """ Write the merged dictionary atomically: temporary file in the output directory, then rename, so a failure never
    leaves a truncated dictionary behind. An existing file keeps its mode; a new one gets the umask default. Non-regular
    outputs (e.g. /dev/stdout, a FIFO) cannot be renamed over and are written directly. """
    text = json.dumps(merged, indent=2)
    if path.exists() and not path.is_file():
        with open(path, "w") as output_fh:
            output_fh.write(text)
        return
    if path.is_file():
        mode = stat.S_IMODE(path.stat().st_mode)
    else:
        umask = os.umask(0)
        os.umask(umask)
        mode = 0o666 & ~umask
    temporary = tempfile.NamedTemporaryFile(mode="w", dir=str(path.parent), prefix=f"{path.name}.", suffix=".tmp",
                                            delete=False)
    try:
        with temporary:
            temporary.write(text)
            temporary.flush()
            os.fsync(temporary.fileno())
        os.chmod(temporary.name, mode)
        os.replace(temporary.name, path)
    except BaseException:
        try:
            os.unlink(temporary.name)
        except OSError:
            pass
        raise


def parse_arguments(arguments=None):
    """ Parse arguments for this script """
    parser = argparse.ArgumentParser(
        description="Merge two or more F Prime JSON dictionaries. List the deployment the GDS is attached to first: "
                    "it is primary, its entries come first and it decides the metadata. Entries that are identical in "
                    "several inputs are merged into one. Two entries with the same name but different ids are both "
                    "kept, each renamed '<Deployment>.<name>' where <Deployment> is the last segment of its "
                    "dictionary's deploymentName (disable with --no-namespace).",
        epilog="Merge all original dictionaries in a single invocation; do not merge an already-merged output with "
               "another deployment.")
    parser.add_argument("--name", type=str, default=None,
                        help="Name to use as the new 'deploymentName' field (dotted identifier). Default: all input "
                             "names joined with '_' plus '_merged'")
    parser.add_argument("--output", type=Path, default=Path("MergedAppDictionary.json"),
                        help="Output dictionary path. Default: MergedAppDictionary.json")
    parser.add_argument("--permissive", action="store_true", default=False,
                        help="Ignore version discrepancies between dictionaries (metadata only)")
    parser.add_argument("--prefer-primary", action="store_true", default=False,
                        help="On an id collision or a differing definition, keep the entry of the earliest dictionary "
                             "holding it and drop the later one with a warning")
    parser.add_argument("--no-namespace", action="store_true", default=False,
                        help="Do not rename same-named entries with different ids; report them as errors (or, with "
                             "--prefer-primary, keep the earliest and drop the later one)")
    parser.add_argument("inputs", type=Path, nargs="+", metavar="dictionary",
                        help="Two or more dictionaries in decreasing order of precedence; the first is primary (the "
                             "deployment the GDS is attached to)")

    args = parser.parse_intermixed_args(arguments)  # options may appear between dictionaries: 'd1 d2 --flag d3'
    if len(args.inputs) < 2:
        parser.error("at least two dictionaries are required")

    # Validate arguments
    if args.name is not None and not IDENT_RE.fullmatch(args.name):
        raise ValueError(f"--name '{args.name}' is an invalid identifier")
    for dictionary in args.inputs:
        if not dictionary.exists():
            raise ValueError(f"'{dictionary}' does not exist")
    return args


def main(arguments=None):
    """ Main entry point: exit 0 on success (warnings printed), 1 on any error, 2 on a usage error """
    try:
        args = parse_arguments(arguments)
        inputs = [load_input(index, path) for index, path in enumerate(args.inputs, start=1)]
    except Exception as exception:
        print(f"[ERROR] {exception}", file=sys.stderr)
        sys.exit(1)
    opts = MergeOptions(name=args.name, permissive=args.permissive, prefer_primary=args.prefer_primary,
                        no_namespace=args.no_namespace)
    merged, report, _ = merge_all(inputs, opts)
    report.print()
    if merged is None:
        print(f"[ERROR] Merge failed with {len(report.errors)} error(s) and {len(report.warnings)} warning(s); no "
              f"output written", file=sys.stderr)
        sys.exit(1)
    if report.warnings:
        print(f"[WARNING] Merged {len(inputs)} dictionaries with {report.summary()}", file=sys.stderr)
    try:
        write_output(args.output, merged)
    except OSError as error:
        print(f"[ERROR] cannot write '{args.output}': {error}", file=sys.stderr)
        sys.exit(1)
    sys.exit(0)


if __name__ == "__main__":
    main()
