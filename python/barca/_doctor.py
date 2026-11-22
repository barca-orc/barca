"""Finite prerequisite probe. No pipeline imports, state synchronization or store writes."""

import getpass
import os
import platform
import sys

from barca import __version__, _helper, _storage


def _local_identity():
    return {
        "provider": "local",
        "subject": getpass.getuser(),
        "source": "operating system user",
        "account": None,
        "recovery": "Check this user's directory permissions.",
    }


def probe(store):
    uri = store["uri"]
    display = _storage.safe_error(uri)
    data = {"enabled": True, "uri": display, "identity": None}
    try:
        local = _storage.local_path_of(uri)
        if local is not None:
            ancestor = local
            while not ancestor.exists() and ancestor.parent != ancestor:
                ancestor = ancestor.parent
            _storage.check_store(str(ancestor))
            data["identity"] = _local_identity()
            detail = (
                "store directory is reachable"
                if ancestor == local
                else "new store; existing parent directory is reachable"
            )
        else:
            _storage.check_store(uri)
            # Provider identity is resolved by the diagnostic helper, never by importing
            # pipeline modules. Filled by the shared identity adapter below.
            from barca import _identity

            data["identity"] = _identity.resolve(uri)
            identity = data["identity"]
            subject = identity.get("subject") or "effective principal unresolved"
            account = identity.get("tenant") or identity.get("account")
            detail = f"store is reachable; identity: {subject} ({identity['source']})"
            if account:
                detail += f"; account/tenant: {account}"
        if local is not None:
            detail += f"; identity: {data['identity']['subject']} (operating system user)"
        return {"name": store["name"], "ok": True, "detail": detail, "data": data}
    except Exception as exc:
        if _storage.is_remote(uri):
            from barca import _identity

            data["identity"] = _identity.context(uri)
        return {
            "name": store["name"],
            "ok": False,
            "detail": _storage.describe_error(uri, exc),
            "data": data,
        }


def main():
    # Stdin is the lifeline (`run_to_output`), so the request is in the environment.
    request = _helper.env_request("BARCA_DOCTOR_REQUEST")

    def report():
        implementation = sys.implementation.version
        implementation_version = ".".join(str(part) for part in implementation[:3])
        if implementation.releaselevel != "final":
            implementation_version += implementation.releaselevel[0] + str(implementation.serial)
        return {
            "python": {
                "executable": sys.executable,
                "version": platform.python_version(),
                "barca_version": __version__,
            },
            "markers": {
                "implementation_name": sys.implementation.name,
                "implementation_version": implementation_version,
                "os_name": os.name,
                "platform_machine": platform.machine(),
                "platform_python_implementation": platform.python_implementation(),
                "platform_release": platform.release(),
                "platform_system": platform.system(),
                "platform_version": platform.version(),
                "python_full_version": platform.python_version(),
                "python_version": ".".join(platform.python_version_tuple()[:2]),
                "sys_platform": sys.platform,
            },
            "stores": [probe(store) for store in request["stores"]],
        }

    _helper.answer(report, newline=True)
    return 0


if __name__ == "__main__":
    _helper.run(main, lifeline=True)
