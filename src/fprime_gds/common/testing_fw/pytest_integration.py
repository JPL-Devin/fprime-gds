"""pytest_integration.py: F´ fixture API fixture

pytest uses fixtures to provide for extra functionality when writing tests. This fixture sets up the F´ stack allowing
our tests to use a configured test API without needing to create any specific setup to use that API. i.e. write a test
as follows:

```python
def test_my_test(fprime_test_api):
    ''' Perform my test '''
    fprime_test_api.send_and_assert_command(...)
```

Here a test (defined by starting the name with test_) uses the fprime_test_api fixture to perform the test.

@author lestarch
"""

import argparse
import itertools
import os
import shlex
import sys
from pathlib import Path
import pytest

from fprime_gds.common.testing_fw.api import IntegrationTestAPI
from fprime_gds.executables.cli import StandardPipelineParser, ConfigDrivenParser

SEQUENCE_COUNTER = itertools.count()


def explicit_pipeline_arguments(config, pipeline_parser):
    """Standard-pipeline arguments the user explicitly supplied to pytest

    pytest's parsed namespace carries argparse defaults for every option registered in pytest_addoption; feeding those
    back to ConfigDrivenParser would override the configuration file. Instead, re-parse the pytest invocation (addopts
    ini value, PYTEST_ADDOPTS, then the command line, in pytest's order) with defaults suppressed so that only the
    flags actually given are reproduced.

    Args:
        config: pytest config object
        pipeline_parser: StandardPipelineParser whose options are mirrored into pytest

    Return:
        list of command line arguments for the explicitly supplied standard-pipeline options
    """
    explicit_parser = argparse.ArgumentParser(
        add_help=False, argument_default=argparse.SUPPRESS
    )
    for flags, specifiers in pipeline_parser.get_arguments().items():
        flags = [flag for flag in flags if flag.startswith("--")]
        # Flags (store_true/store_false) keep their default so reproduce_cli_args only emits them when toggled
        if specifiers.get("action", "store") == "store":
            specifiers = {
                key: value for key, value in specifiers.items() if key != "default"
            }
        explicit_parser.add_argument(*flags, **specifiers)
    invocation = (
        list(config.getini("addopts"))
        + shlex.split(os.environ.get("PYTEST_ADDOPTS", ""))
        + list(config.invocation_params.args)
    )
    explicit_ns, _ = explicit_parser.parse_known_args(invocation)
    return pipeline_parser.reproduce_cli_args(explicit_ns)


def pytest_addoption(parser):
    """Add fprime-gds options to the pytest parser

    Pytest allows users to add options to its parser. These options act very similar to argparse options and thus can be
    reused from the standard GDS command line processing. Note: pytest restricts the use of short flags (-[a-z]) thus we
    strip those from the standard cli processing. Long options must be supplied when testing using pytest.

    Args:
        parser: pytest style parser. Use "addoption" to add an option to it.
    """
    for flags, specifiers in StandardPipelineParser().get_arguments().items():
        # Reduce flags to only the long option (i.e. --something) form
        flags = [flag for flag in flags if flag.startswith("--")]
        parser.addoption(*flags, **specifiers)

    # Add an option to specify JUnit XML report file
    parser.addoption(
        "--junit-xml-file",
        action="store",
        default="report.xml",
        help="File to store JUnit XML report. [default: %(default)s]",
    )
    # Add an option to enable JUnit XML report generation to a specified location
    parser.addoption(
        "--gen-junitxml",
        action="store_true",
        help="Enable JUnitXML report generation to a specified location",
    )
    # Add an option to specify a json config file that maps components
    parser.addoption(
        "--deployment-config",
        action="store",
        type=Path,
        help="Path to JSON configuration file for mapping deployment components",
    )

    parser.addoption(
        "--use-yamcs",
        action="store_true",
        help="Use YAMCS transport instead of TCP socket",
    )
    parser.addoption(
        "--yamcs-url",
        action="store",
        default="http://localhost:8090",
        help="YAMCS server URL [default: %(default)s]",
    )


def pytest_configure(config):
    """This is a hook to allow plugins and conftest files to perform initial configuration

    This hook is called for every initial conftest file after command line options have been parsed. After that, the
    hook is called for other conftest files as they are registered.
    """
    # Create a JUnit XML report file to capture the test result in a specified location
    if config.getoption("--gen-junitxml"):
        logs = config.getoption("--logs")
        if not logs:
            raise pytest.UsageError("--gen-junitxml requires --logs to be specified")
        config.option.xmlpath = Path(logs) / config.getoption("--junit-xml-file")


@pytest.fixture(scope="session")
def fprime_test_api_session(request):
    """Create a session-level fprime test API

    This is a pytest session fixture. Using the options added above, this will parse the necessary options for
    connecting the standard pipeline to the running GDS. Options not given on the pytest command line are read from
    the fprime-gds configuration file ($FPRIME_GDS_CONFIG_PATH, else ./fprime-gds.yml), as `fprime-gds` does; the
    plugin exposes no --config flag, so the file is selected only through that variable or the working directory.
    This pipeline is supplied to the fprime test API returned as the result of this fixture. This has several
    implications:
      1. APIs all use one connection to the GDS
      2. APIs and the connections are live across the whole pytest session. See fprime_test_api.

    Note: the implementation uses yield in order to setup the object and tear it down afterwards. This is as-recommended
    from pytest documentation.

    Args:
        request: standard pytest-supplied harness used for processing CLI arguments

    Return:
        fprime test API connected to the GDS.  Note: a second call will shut down that object.
    """
    pipeline_parser = StandardPipelineParser()

    # ConfigDrivenParser fills options from the configuration file first and applies the supplied arguments over them
    # (matching fprime-gds CLI behavior). It must not be handed sys.argv (pytest's own command line) nor pytest's parsed
    # namespace (which carries argparse defaults that would override the file), only the flags the user gave.
    arg_ns, _, _ = ConfigDrivenParser.parse_known_args(
        [StandardPipelineParser],
        arguments=explicit_pipeline_arguments(request.config, pipeline_parser),
        client=True,
    )

    pipeline = None
    api = None
    deployment_config = None
    try:

        if request.config.getoption("--use-yamcs"):
            try:
                from fprime_gds.common.yamcs_transport import YamcsClient
            except ImportError:
                raise pytest.UsageError(
                    "--use-yamcs requires the yamcs-client package. Install with: pip install fprime-gds[yamcs]"
                )
            yamcs_url = request.config.getoption("--yamcs-url")
            scheme = "yamcs+https" if yamcs_url.startswith("https://") else "yamcs"
            yamcs_host = yamcs_url.replace("http://", "").replace("https://", "")

            arg_ns.connection_transport = YamcsClient
            arg_ns.connection_uri = f"{scheme}://{yamcs_host}"

        pipeline = pipeline_parser.pipeline_factory(arg_ns, pipeline)

        # Get deployment configuration from command line arguments
        if request.config.option.deployment_config:
            deployment_config = request.config.option.deployment_config

        # Build and set up the integration test api
        api = IntegrationTestAPI(pipeline, deployment_config, arg_ns.logs)
        api.setup()

        # Return the API. Note: the second call here-in will begin after the yield and clean-up after the test
        yield api
    # In all cases, whether setup erred or proceeded to the yield, the teardown is now necessary
    finally:
        # Attempt to teardown the API to ensure we are clean after the fixture is created
        try:
            if api is not None:
                api.teardown()
        except Exception as exc:
            print(f"[WARNING] Exception in API teardown: {exc}", file=sys.stderr)
        # Attempt to shut down the pipeline connection
        try:
            if pipeline is not None:
                pipeline.disconnect()
        except Exception as exc:
            print(f"[WARNING] Exception in pipeline teardown: {exc}", file=sys.stderr)


@pytest.fixture(scope="function")
def fprime_test_api(fprime_test_api_session, request):
    """Provide a per-testcase fixture

    Although the test API should exist across all testcases and thus be created at the "session" level, individual test
    cases need a clean test API that has logged the testcases has started. Thus, the session API is refined into a
    test case API by performing the test case setup consisting of:

    1. clearing current API history
    2. logging the test case name

    Args:
        fprime_test_api_session: session API to adjust for each test case
        request: pytest supplied request to get test name

    Return:
        test case specific session (identical to full session)
    """
    fprime_test_api_session.start_test_case(request.node.name, next(SEQUENCE_COUNTER))
    return fprime_test_api_session
