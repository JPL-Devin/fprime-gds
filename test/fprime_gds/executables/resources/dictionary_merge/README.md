# dictionary_merge test fixtures

`DeploymentATopologyDictionary.json`, `DeploymentBTopologyDictionary.json`
: Unmodified `fpp-to-dict` output of the two deployments of
  [fprime-generic-hub-reference](https://github.com/JPL-Devin/fprime-generic-hub-reference) at commit `a32988b`
  (F Prime `v4.2.0`), taken from
  `build-artifacts/Linux/FprimeGenericHubReference_Deployments_Deployment<X>/dict/` after

  ```bash
  fprime-util generate && fprime-util build -j"$(nproc)"    # in FprimeGenericHubReference/Deployments/Deployment<X>
  ```

  Both deployments instantiate the `CdhCore`, `ComCcsds`, `FileHandling` and `DataProducts` subtopologies at the same
  base ids and reuse local base ids for their deployment-local instances, which is what the merge tests exercise.

`GroundChannels.json`
: The hand-written ground dictionary from the F Prime how-to
  [Derive channels on the ground](https://fprime.jpl.nasa.gov/latest/docs/how-to/operate/derive-channels-on-ground/)
  (`docs/how-to/operate/derive-channels-on-ground.md` in nasa/fprime).

`GroundChannelsMinimal.json`
: `GroundChannels.json` with every metadata field except `dictionarySpecVersion` removed.

`expected/ground_channels_merged.json`
: `fprime-merge-dictionary --permissive DeploymentATopologyDictionary.json GroundChannels.json` as produced by the
  tool before this rewrite (fprime-gds `7972f6f`); the golden for the unchanged two-input behaviour.
