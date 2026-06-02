# FPrime GDS tools
## fprime-prm
Consolidated CLI for encoding and decoding F Prime parameter database files.

### JSON file reference
JSON files for parameter encoding should take the following form:
```json
{
    "componentInstanceOne": {
        "parameterNameOne": "parameter value",
        "parameterNameTwo": ["a", "b", "c"],
        "parameterNameThree": {
            "complexValue": [123, 456]
        }
    },
    "componentInstanceTwo": {
        "parameterNameFour": true
    }
}
```
The JSON should consist of a key-value map of component instance names to an inner key-value map. The inner key-value map should consist of parameter name-to-value map entries. The parameter values support complex FPrime types, such as nested structs, arrays or enum constants. Structs are instantiated with key-value maps, where the keys are the field names and the values are the field values. Arrays are just JSON arrays, and enum constants are represented as strings.

### Encoding: Initialize a ParamDB .dat File

The `fprime-prm encode` command can be used to create a `.dat` file compatible with the `PrmDb` component from a json file. To use, create a compatible JSON file as defined in the JSON File Reference above, and pass it in to the tool using the `dat` subcommand, like so:
```
fprime-prm encode dat <json file> --dictionary <path to compiled FPrime dict>
```
You should then have a `.dat` file which can be passed in to the `PrmDb`. Note, this `.dat` file will only have entries for the parameters specified in the JSON file. If you want it to instead have a value for all parameters which have a default value, you can add the `--defaults` option. Then, the generated `.dat` file will essentially reset all parameters back to default, except for those specified in the JSON file.

### Encoding: Create a .seq File From a Parameter JSON File
Sometimes, you may want to update parameters while the FPrime application is running. This can be accomplished with a sequence of `_PRM_SET` commands, which `fprime-prm encode` can automatically create for you. To use, create a compatible JSON file as defined in the JSON File Reference above, and pass it in to the tool using the `seq` subcommand, like so:
```
fprime-prm encode seq <json file> --dictionary <path to compiled FPrime dict>
```
You should then have a `.seq` file which can be compiled and executed by the `CmdSequencer`.

### Decoding: Read a ParamDB .dat File
The `fprime-prm decode` command decodes binary parameter database (`.dat`) files into human-readable formats. To use:
```
fprime-prm decode <dat file> --dictionary <path to compiled FPrime dict>
```
Supported output formats (via `--format` / `-f`):
- `json` (default): Round-trip compatible with the encode input format
- `text`: Human-readable format with component grouping, types, and IDs
- `csv`: Spreadsheet-compatible format with columns: Component, Parameter, Value, Type, ID

### Legacy commands
The `fprime-prm-write` and `fprime-prm-decode` commands are still available as aliases for backwards compatibility.
