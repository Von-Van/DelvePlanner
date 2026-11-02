"""Entry script for a frozen (PyInstaller) build of the agent. Absolute imports only.

Runners are loaded by name at run time, which a freezer can't see, so every runner the build
should offer is imported here. Add the agent's own runner modules next to the diagnostic one.
"""

import sys

import delveagent.protocol.diagnostic  # noqa: F401  (the link's test runner)
from delveagent.protocol.__main__ import main

if __name__ == "__main__":
    sys.exit(main())
