"""Settings every test process needs before any test module is imported."""

import os

# gcsfs >= 2026.10 defaults to an experimental mode that asks the gRPC Storage Control API for a
# bucket's type, and fake-gcs-server only speaks HTTP, so the call hangs. gcsfs reads the toggle
# once, when it is first imported, so it has to be set before any test can import it: a test
# that set it later for itself stalled whenever an earlier test had imported gcsfs first.
os.environ.setdefault("GCSFS_EXPERIMENTAL_ZB_HNS_SUPPORT", "false")
