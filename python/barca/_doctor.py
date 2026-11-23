"""Finite prerequisite probe. No pipeline imports, state synchronization or store writes."""

import getpass
import platform
import sys
import tomllib
from pathlib import Path

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


def project_version(root, running):
    """Check the CLI version with upstream PEP 508/440 parsing in this finite helper."""
    path = Path(root) / "pyproject.toml"
    if not path.exists():
        return None

    def check():
        # Keep the standards parser out of normal workers and the native coordinator.
        from barca._vendor.packaging.requirements import InvalidRequirement, Requirement
        from barca._vendor.packaging.utils import canonicalize_name
        from barca._vendor.packaging.version import Version

        project = tomllib.loads(path.read_text()).get("project", {})
        dependencies = project.get("dependencies") if isinstance(project, dict) else None
        if dependencies is None:
            return "no declared project.dependencies requirement for barca"
        if not isinstance(dependencies, list):
            raise ValueError("project.dependencies must be an array")
        version = Version(running)
        matched = []
        for text in dependencies:
            if not isinstance(text, str):
                raise ValueError("project.dependencies entries must be strings")
            try:
                requirement = Requirement(text)
            except InvalidRequirement:
                # Do not echo an invalid requirement that might contain credentials.
                raise ValueError(
                    "invalid project.dependencies requirement; use PEP 508 syntax"
                ) from None
            if canonicalize_name(requirement.name) != "barca":
                continue
            if requirement.marker is not None and not requirement.marker.evaluate():
                continue
            if requirement.url is not None:
                raise ValueError(
                    "cannot verify a direct URL barca requirement against running barca "
                    f"{running}; declare a version requirement to check it"
                )
            # The CLI is already installed: check its supplied version, including a
            # prerelease, rather than applying an installer's candidate-selection policy.
            if not requirement.specifier.contains(version, prereleases=True):
                raise ValueError(
                    f"running barca {running} does not satisfy {text}; "
                    "sync the project environment and run its barca"
                )
            matched.append(text)
        if not matched:
            return "no active project.dependencies requirement for barca"
        return f"running barca {running} satisfies {', '.join(matched)}"

    try:
        return {"ok": True, "detail": check()}
    except (OSError, ValueError, KeyError) as exc:
        return {"ok": False, "detail": str(exc)}


def main():
    # Stdin is the lifeline (`run_to_output`), so the request is in the environment.
    request = _helper.env_request("BARCA_DOCTOR_REQUEST")

    def report():
        return {
            "python": {
                "executable": sys.executable,
                "version": platform.python_version(),
                "barca_version": __version__,
            },
            "project_version": project_version(request["project_root"], request["cli_version"]),
            "stores": [probe(store) for store in request["stores"]],
        }

    _helper.answer(report, newline=True)
    return 0


if __name__ == "__main__":
    _helper.run(main, lifeline=True)
