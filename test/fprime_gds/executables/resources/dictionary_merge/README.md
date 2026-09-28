# dictionary_merge test fixtures

`DeploymentATopologyDictionary.json`, `DeploymentBTopologyDictionary.json`
: `fpp-to-dict` output of the two deployments of
  [fprime-generic-hub-reference](https://github.com/JPL-Devin/fprime-generic-hub-reference) at commit `a32988b`
  (F Prime `v4.2.0`), taken from
  `build-artifacts/Linux/FprimeGenericHubReference_Deployments_Deployment<X>/dict/` after

  ```bash
  fprime-util generate && fprime-util build -j"$(nproc)"    # in FprimeGenericHubReference/Deployments/Deployment<X>
  ```

  trimmed to the entries of a few components, with `typeDefinitions` reduced to the types those entries reference
  (`constants` and `metadata` untouched, entries themselves unmodified):

  | component | in | why |
  |---|---|---|
  | `CdhCore.cmdDisp`, `CdhCore.events` | A, B | shared subtopology at the same base id: identical names, ids and bodies |
  | `<X>.c_comp` | A, B | deployment-local instances with different names at the same id (`HubCommandTest`, `HubCommandTestEvr`) |
  | `<A>.a_rateGroup1` / `<B>.b_rateGroup1` | A / B | same, for events and channels |
  | `<A>.a_comp` | A | unique to one deployment |

  That gives 7 commands / 17 events / 3 channels shared and 1 / 3 / 2 id clashes, the two situations a real hub
  deployment produces; the full dictionaries (about 47 / 214 / 97 entries each) add nothing but more of the same. To
  re-trim from fresh `fpp-to-dict` output, keep the entries whose component (name minus its last segment) is in that
  table, then keep the transitive closure of `typeDefinitions` reachable through `{"kind": "qualifiedIdentifier"}`
  references.

`GroundChannels.json`
: The hand-written ground dictionary from the F Prime how-to
  [Derive channels on the ground](https://fprime.jpl.nasa.gov/latest/docs/how-to/operate/derive-channels-on-ground/)
  (`docs/how-to/operate/derive-channels-on-ground.md` in nasa/fprime).

`GroundChannelsMinimal.json`
: `GroundChannels.json` with every metadata field except `dictionarySpecVersion` removed.
