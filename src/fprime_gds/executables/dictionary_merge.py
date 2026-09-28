""" fprime_gds.executables.dictionary_merge: merge two or more F Prime JSON dictionaries

The first dictionary is primary: its entries come first and it decides the metadata. Within the id-bearing sections
(commands, parameters, events, telemetryChannels, records, containers) an arriving entry is:

    1. identical to a held entry (same name, id and body)  -> merged into it
    2. same id as a held entry                              -> error; --prefer-primary keeps the earliest, drops it
    3. same name as a held entry, different id              -> both renamed '<prefix>.<name>'; --no-namespace: error
    4. a name that was already renamed away                 -> appended as '<prefix>.<name>'
    5. otherwise                                            -> appended unchanged

`prefix` is the last segment of the input's metadata.deploymentName or its --prefix value. --namespace-all renames
every entry of those sections up front. Types and constants are never renamed: a differing definition is an error
unless --prefer-primary keeps the earliest. Packet-set channel references follow renames; a packet naming a dropped
channel is removed. All errors are collected before anything is written. Merge the original dictionaries in one
invocation: an already-merged output holds prefixed names that clash by id with its own inputs.
"""

from __future__ import annotations

import argparse
import copy
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path

IDENT_RE = re.compile(r"[A-Za-z_][A-Za-z_0-9]*(\.[A-Za-z_][A-Za-z_0-9]*)*")
UNIQUE_SECTIONS = {"commands": "opcode", "parameters": "id", "events": "id", "telemetryChannels": "id",
                   "records": "id", "containers": "id"}
NON_UNIQUE_SECTIONS = ("typeDefinitions", "constants")
PACKET_SECTION = "telemetryPacketSets"
SECTION_ORDER = ["metadata", *NON_UNIQUE_SECTIONS, *UNIQUE_SECTIONS, PACKET_SECTION]
VERSION_FIELDS = ("projectVersion", "frameworkVersion", "dictionarySpecVersion")


@dataclass
class MergeOptions:
    name: str | None = None
    permissive: bool = False
    prefer_primary: bool = False
    no_namespace: bool = False
    namespace_all: bool = False


@dataclass
class MergeReport:
    """ Errors and warnings in generation order """
    errors: list[str] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)
    lines: list[str] = field(default_factory=list)

    def error(self, message):
        self.errors.append(message)
        self.lines.append(f"[ERROR] {message}")

    def warning(self, message):
        self.warnings.append(message)
        self.lines.append(f"[WARNING] {message}")


@dataclass
class LoadedInput:
    """ One input: 1-based position, display path, parsed content, --prefix value (None: use deploymentName) """
    index: int
    path: str
    data: dict
    prefix_override: str | None = None

    def prefix(self):
        if self.prefix_override is not None:
            return self.prefix_override
        name = self.data["metadata"].get("deploymentName")
        return name.rsplit(".", 1)[-1] if isinstance(name, str) else None


@dataclass
class Slot:
    """ A held entry: the input it came from, the name it arrived with, and every input merged into it """
    entry: dict
    origin: int
    arrival: str
    contributors: dict[int, str]


class Section:
    """ Held entries of one section with the indices needed to classify an arriving entry """

    def __init__(self, name, id_key):
        self.name = name
        self.id_key = id_key
        self.slots: list[Slot] = []
        self.by_name: dict[str, int] = {}
        self.by_id: dict[int, int] = {}
        self.by_arrival: dict[str, list[int]] = {}

    def add(self, entry, origin, arrival):
        idx = len(self.slots)
        self.slots.append(Slot(entry, origin, arrival, {origin: arrival}))
        self.by_name[entry["name"]] = idx
        if self.id_key:
            self.by_id[entry[self.id_key]] = idx
        self.by_arrival.setdefault(arrival, []).append(idx)

    def entries(self):
        return [slot.entry for slot in self.slots]


def renamed(entry, name):
    """ Copy of an entry under another name, key order preserved """
    return {key: (name if key == "name" else value) for key, value in entry.items()}


def is_int(value):
    return isinstance(value, int) and not isinstance(value, bool)


def check_structure(inp: LoadedInput, report: MergeReport):
    """ Every section present with the right shape; entries carry a string name and an integer id """
    bad = f"Malformed dictionary '{inp.path}':"
    if not isinstance(inp.data, dict):
        report.error(f"{bad} not a JSON object")
        return
    if not isinstance(inp.data.get("metadata"), dict):
        report.error(f"{bad} 'metadata' must be an object")
    for section in SECTION_ORDER[1:]:
        if not isinstance(inp.data.get(section), list):
            report.error(f"{bad} '{section}' must be an array")
            continue
        name_key = "qualifiedName" if section in NON_UNIQUE_SECTIONS else "name"
        id_key = UNIQUE_SECTIONS.get(section)
        for entry in inp.data[section]:
            if not (isinstance(entry, dict) and isinstance(entry.get(name_key), str)
                    and (id_key is None or is_int(entry.get(id_key)))):
                report.error(f"{bad} entry in '{section}' must have string '{name_key}'"
                             f"{f' and integer {id_key!r}' if id_key else ''}: {json.dumps(entry)[:80]}")


def merge_metadata(inputs: list[LoadedInput], opts: MergeOptions, report: MergeReport):
    primary = inputs[0].data["metadata"]
    for inp in inputs[1:]:
        metadata = inp.data["metadata"]
        if opts.permissive:
            continue
        for key in VERSION_FIELDS:
            if primary.get(key) != metadata.get(key):
                report.error(f"metadata: Inconsistent metadata values for field '{key}' ({primary.get(key)} vs "
                             f"{metadata.get(key)}); use --permissive to ignore")
        if "libraryVersions" in primary and primary["libraryVersions"] != metadata.get("libraryVersions"):
            report.warning(f"metadata: libraryVersions differ between '{inputs[0].path}' and '{inp.path}'")
    names = [str(inp.data["metadata"].get("deploymentName", "unknown")) for inp in inputs]
    return {**primary, "deploymentName": opts.name or "_".join(names) + "_merged"}


def merge_non_unique(inputs: list[LoadedInput], section, opts: MergeOptions, report: MergeReport):
    """ typeDefinitions / constants keyed by qualifiedName: identical collapse, differing error or keep-first """
    held: dict[str, tuple] = {}
    for inp in inputs:
        for entry in inp.data[section]:
            name = entry["qualifiedName"]
            previous = held.setdefault(name, (entry, inp.path))
            if previous[0] != entry:
                if opts.prefer_primary:
                    report.warning(f"{section}: '{name}' differs in '{inp.path}'; kept definition from "
                                   f"'{previous[1]}'")
                else:
                    report.error(f"{section}: '{name}' has inconsistent definitions in '{previous[1]}' and "
                                 f"'{inp.path}'; use --prefer-primary to keep the first")
    return [entry for entry, _ in held.values()]


class Merger:
    """ State of one N-way merge of the id-bearing sections and the packet sets """

    def __init__(self, inputs: list[LoadedInput], opts: MergeOptions, report: MergeReport):
        self.inputs, self.opts, self.report = inputs, opts, report
        self.renames: dict[int, dict[str, dict[str, str]]] = {
            inp.index: {section: {} for section in [*UNIQUE_SECTIONS, PACKET_SECTION]} for inp in inputs}
        self.dropped_channels: dict[int, set] = {inp.index: set() for inp in inputs}
        self.bad_prefix: set = set()

    def path(self, k):
        return self.inputs[k - 1].path

    def prefix(self, k, needed_for):
        """ Validated namespace prefix of input k, or None (reported once per input) """
        inp = self.inputs[k - 1]
        prefix = inp.prefix()
        if prefix is not None and IDENT_RE.fullmatch(prefix):
            return prefix
        if k not in self.bad_prefix:
            self.bad_prefix.add(k)
            self.report.error(f"'{inp.path}': cannot derive a namespace prefix from metadata.deploymentName "
                              f"{inp.data['metadata'].get('deploymentName')!r} ({needed_for}); pass --prefix or "
                              f"--no-namespace")
        return None

    def check_distinct_prefixes(self):
        seen: dict[str, int] = {}
        for inp in self.inputs:
            prefix = self.prefix(inp.index, "needed for --namespace-all")
            if prefix in seen:
                self.report.error(f"'{self.path(seen[prefix])}' and '{inp.path}' both have the namespace prefix "
                                  f"'{prefix}'; --namespace-all needs distinct prefixes")
            elif prefix is not None:
                seen[prefix] = inp.index

    def target(self, section: Section, k, name):
        """ '<prefix>.<name>' for input k's entry if the prefix is valid and the name is free, else None """
        prefix = self.prefix(k, f"needed to rename '{name}'")
        if prefix is None:
            return None
        target = f"{prefix}.{name}"
        if target in section.by_name:
            other = section.slots[section.by_name[target]]
            self.report.error(f"{section.name}: cannot rename '{name}' from '{self.path(k)}' to '{target}': that name "
                              f"is already defined in '{self.path(other.origin)}'")
            return None
        return target

    def attach(self, section: Section, idx, k, arrival):
        slot = section.slots[idx]
        slot.contributors[k] = arrival
        if slot.entry["name"] != arrival:
            self.renames[k][section.name][arrival] = slot.entry["name"]

    def rename(self, section: Section, idx, name):
        slot = section.slots[idx]
        del section.by_name[slot.entry["name"]]
        slot.entry["name"] = name
        section.by_name[name] = idx
        for j, arrival in slot.contributors.items():
            self.renames[j][section.name][arrival] = name

    def add_prefixed(self, section: Section, k, entry):
        target = self.target(section, k, entry["name"])
        if target is not None:
            section.add(renamed(entry, target), k, entry["name"])
            self.renames[k][section.name][entry["name"]] = target
        return target

    def classify(self, section: Section, k, entry):
        """ Place one entry of input k according to the rules in the module docstring """
        opts, report, id_key = self.opts, self.report, section.id_key
        n, path = entry["name"], self.path(k)
        i = entry[id_key] if id_key else None
        id_of = (lambda e: f"{id_key} 0x{e[id_key]:X}") if id_key else (lambda e: "definition")

        # 1. identical entry held under the same arrival name (bare or renamed)
        for idx in section.by_arrival.get(n, []):
            held = section.slots[idx]
            if held.entry == renamed(entry, held.entry["name"]):
                self.attach(section, idx, k, n)
                return

        # 2. id held: same arrival name is a differing definition of one item, anything else is a clash
        if id_key and i in section.by_id:
            idx = section.by_id[i]
            held = section.slots[idx]
            origin, held_name = self.path(held.origin), held.entry["name"]
            if held.arrival == n and opts.prefer_primary:
                report.warning(f"{section.name}: '{n}' ({id_of(entry)}) differs in '{path}'; kept definition "
                               f"'{held_name}' from '{origin}'")
                self.attach(section, idx, k, n)
            elif held.arrival == n:
                report.error(f"{section.name}: '{n}' ({id_of(entry)}) has different definitions in '{origin}' and "
                             f"'{path}'; use --prefer-primary to keep the first")
            elif opts.prefer_primary:
                report.warning(f"{section.name}: {id_of(entry)}: '{n}' from '{path}' dropped in favour of "
                               f"'{held_name}' from '{origin}'")
                self.drop(section, k, n)
            else:
                report.error(f"{section.name}: {id_of(entry)} is used by '{held_name}' in '{origin}' and '{n}' in "
                             f"'{path}'; use --prefer-primary to keep the first")
            return

        if opts.namespace_all and id_key:
            self.add_prefixed(section, k, entry)
            return

        # 3. same name, different id
        if n in section.by_name:
            idx = section.by_name[n]
            held = section.slots[idx]
            origin = self.path(held.origin)
            if opts.no_namespace and opts.prefer_primary:
                report.warning(f"{section.name}: '{n}' ({id_of(entry)}) from '{path}' dropped in favour of '{n}' "
                               f"({id_of(held.entry)}) from '{origin}'")
                self.drop(section, k, n)
                return
            if opts.no_namespace:
                report.error(f"{section.name}: '{n}' is defined in '{origin}' ({id_of(held.entry)}) and '{path}' "
                             f"({id_of(entry)}) with different {id_key or 'definition'}s; use --prefer-primary to "
                             f"keep the first, or drop --no-namespace to keep both under '<prefix>.'-qualified names")
                return
            if held.entry["name"] != held.arrival:
                report.error(f"{section.name}: '{n}' from '{path}' collides with the namespaced '{n}' from '{origin}'")
                return
            target_h, target_k = self.target(section, held.origin, n), self.target(section, k, n)
            if target_h is None or target_k is None:
                return
            if target_h == target_k:
                report.error(f"{section.name}: '{n}' differs in '{origin}' and '{path}' but both have the namespace "
                             f"prefix '{target_k[:-len(n) - 1]}'; use distinct --prefix values or --no-namespace")
                return
            self.rename(section, idx, target_h)
            section.add(renamed(entry, target_k), k, n)
            self.renames[k][section.name][n] = target_k
            report.warning(f"{section.name}: '{n}' has {id_of(held.entry)} in '{origin}' and {id_of(entry)} in "
                           f"'{path}'; renamed to '{target_h}' and '{target_k}' (--no-namespace makes this an error)")
            return

        # 4. name already renamed away earlier in this run
        if not opts.no_namespace and n in section.by_arrival:
            existing = section.slots[section.by_arrival[n][0]].entry["name"]
            target = self.add_prefixed(section, k, entry)
            if target is not None:
                report.warning(f"{section.name}: '{n}' ({id_of(entry)}) from '{path}' renamed to '{target}' (name "
                               f"already namespaced as '{existing}')")
            return

        # 5. new name and id
        section.add(dict(entry), k, n)

    def drop(self, section: Section, k, name):
        if section.name == "telemetryChannels":
            self.dropped_channels[k].add(name)

    def merge_unique(self):
        sections = {name: Section(name, id_key) for name, id_key in UNIQUE_SECTIONS.items()}
        for inp in self.inputs:
            for name, section in sections.items():
                for entry in inp.data[name]:
                    self.classify(section, inp.index, entry)
        return sections

    def rewrite_packet_sets(self, inp: LoadedInput):
        """ Apply the input's channel renames and drops to a copy of its packet sets """
        renames, dropped = self.renames[inp.index]["telemetryChannels"], self.dropped_channels[inp.index]
        result = []
        for packet_set in copy.deepcopy(inp.data[PACKET_SECTION]):
            kept = []
            for packet in packet_set.get("members", []):
                lost = [member for member in packet.get("members", []) if member in dropped]
                if lost:
                    self.report.warning(f"{PACKET_SECTION}: packet '{packet_set['name']}/{packet.get('name')}' from "
                                        f"'{inp.path}' removed because it references dropped channel '{lost[0]}'")
                    continue
                packet["members"] = [renames.get(member, member) for member in packet.get("members", [])]
                kept.append(packet)
            if "members" in packet_set:
                packet_set["members"] = kept
            if "omitted" in packet_set:
                packet_set["omitted"] = [renames.get(m, m) for m in packet_set["omitted"] if m not in dropped]
            result.append(packet_set)
        return result

    def merge_packet_sets(self, channels: Section):
        section = Section(PACKET_SECTION, None)
        for inp in self.inputs:
            for packet_set in self.rewrite_packet_sets(inp):
                self.classify(section, inp.index, packet_set)
        if self.report.errors:
            return section
        for slot in section.slots:
            for packet in slot.entry.get("members", []):
                for member in packet.get("members", []):
                    if member not in channels.by_name:
                        self.report.error(f"{PACKET_SECTION}: packet '{slot.entry['name']}/{packet.get('name')}' in "
                                          f"'{self.path(slot.origin)}' references unknown channel '{member}'")
        if len(section.slots) > 1:
            self.report.warning(f"{PACKET_SECTION}: output holds {len(section.slots)} packet sets; the GDS decodes "
                                f"one, select it with --packet-set-name")
        return section


def merge_all(inputs: list[LoadedInput], opts: MergeOptions):
    """ Merge loaded inputs; returns (merged dictionary or None when errors were collected, report) """
    report = MergeReport()
    for inp in inputs:
        check_structure(inp, report)
    if report.errors:
        return None, report
    merger = Merger(inputs, opts, report)
    if opts.namespace_all:
        merger.check_distinct_prefixes()
    metadata = merge_metadata(inputs, opts, report)
    non_unique = {section: merge_non_unique(inputs, section, opts, report) for section in NON_UNIQUE_SECTIONS}
    sections = merger.merge_unique()
    packet_sets = merger.merge_packet_sets(sections["telemetryChannels"])
    if report.errors:
        return None, report
    merged = {}
    for inp in reversed(inputs):
        merged.update(inp.data)
    merged.update({"metadata": metadata, **non_unique, PACKET_SECTION: packet_sets.entries()})
    merged.update({name: section.entries() for name, section in sections.items()})
    return merged, report


def merge_dictionaries(dictionary1, dictionary2, name=None, permissive=False):
    """ Merge two parsed dictionaries with default options; raises ValueError listing every error """
    inputs = [LoadedInput(1, "dictionary1", dictionary1), LoadedInput(2, "dictionary2", dictionary2)]
    merged, report = merge_all(inputs, MergeOptions(name=name, permissive=permissive))
    if merged is None:
        raise ValueError("\n".join(report.errors))
    return merged


def load_input(index, path, prefix=None) -> LoadedInput:
    with open(path, "r") as file_handle:
        return LoadedInput(index, str(path), json.load(file_handle), prefix)


def parse_arguments(arguments=None):
    """ Parse arguments for this script """
    parser = argparse.ArgumentParser(
        description="Merge two or more F Prime JSON dictionaries. The first is primary (list the deployment the GDS is "
                    "attached to first). Identical entries merge into one; same-named entries with different ids are "
                    "both kept as '<prefix>.<name>', prefix being the last segment of the dictionary's deploymentName.")
    parser.add_argument("--name", type=str, default=None,
                        help="New 'deploymentName' (dotted identifier). Default: '<name1>_<name2>_merged'")
    parser.add_argument("--output", type=Path, default=Path("MergedAppDictionary.json"),
                        help="Output dictionary path. Default: MergedAppDictionary.json")
    parser.add_argument("--permissive", action="store_true", help="Ignore metadata version discrepancies")
    parser.add_argument("--prefer-primary", action="store_true",
                        help="On an id collision or differing definition keep the earliest dictionary's entry")
    namespacing = parser.add_mutually_exclusive_group()
    namespacing.add_argument("--no-namespace", action="store_true",
                             help="Report same-named entries with different ids as errors instead of renaming them")
    namespacing.add_argument("--namespace-all", action="store_true",
                             help="Rename every command/parameter/event/channel/record/container to '<prefix>.<name>'")
    parser.add_argument("--prefix", action="append", metavar="PREFIX",
                        help="Namespace prefix to use instead of the deploymentName segment; once per dictionary, in "
                             "order")
    parser.add_argument("inputs", type=Path, nargs="+", metavar="dictionary", help="Dictionaries, primary first")

    args = parser.parse_intermixed_args(arguments)
    if len(args.inputs) < 2:
        parser.error("at least two dictionaries are required")
    if args.prefix is not None:
        if args.no_namespace:
            parser.error("--prefix has no effect with --no-namespace")
        if len(args.prefix) != len(args.inputs):
            parser.error(f"--prefix must be given once per dictionary ({len(args.inputs)}), got {len(args.prefix)}")
        if len(set(args.prefix)) != len(args.prefix) or not all(IDENT_RE.fullmatch(p) for p in args.prefix):
            parser.error("--prefix values must be distinct dotted identifiers")
    if args.name is not None and not IDENT_RE.fullmatch(args.name):
        raise ValueError(f"--name '{args.name}' is an invalid identifier")
    return args


def main(arguments=None):
    """ Exit 0 on success (warnings printed), 1 on any error, 2 on a usage error """
    try:
        args = parse_arguments(arguments)
        prefixes = args.prefix or [None] * len(args.inputs)
        inputs = [load_input(index, path, prefix)
                  for index, (path, prefix) in enumerate(zip(args.inputs, prefixes), start=1)]
    except (OSError, ValueError) as exception:
        print(f"[ERROR] {exception}", file=sys.stderr)
        sys.exit(1)
    opts = MergeOptions(args.name, args.permissive, args.prefer_primary, args.no_namespace, args.namespace_all)
    merged, report = merge_all(inputs, opts)
    for line in report.lines:
        print(line, file=sys.stderr)
    if merged is None:
        print(f"[ERROR] Merge failed with {len(report.errors)} error(s); no output written", file=sys.stderr)
        sys.exit(1)
    try:
        with open(args.output, "w") as output_fh:
            json.dump(merged, output_fh, indent=2)
    except OSError as error:
        print(f"[ERROR] cannot write '{args.output}': {error}", file=sys.stderr)
        sys.exit(1)
    sys.exit(0)


if __name__ == "__main__":
    main()
