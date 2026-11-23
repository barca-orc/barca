# Vendored Python code

`packaging/` is the unmodified PyPA packaging 26.3 package from the published wheel:

- Source: https://github.com/pypa/packaging/tree/26.3
- Artifact: https://files.pythonhosted.org/packages/63/34/ba1c580383c9eada3711951fef0795c80b829a078d72188184bcab9dd527/packaging-26.3-py3-none-any.whl
- SHA-256: `d7193f7c8e4e93f444fde0262bf90af30e16fa0ad0ad44cb553c87339b23cd1c`
- License: Apache-2.0 OR BSD-2-Clause; original `LICENSE`, `LICENSE.APACHE` and
  `LICENSE.BSD` are retained beside the source and included in Barca's wheel/sdist.

No external Python dependency or import alias is required: upstream uses relative imports.
The doctor helper imports it lazily for PEP 508 requirements, PEP 440 versions/specifiers
and interpreter markers. Ordinary imports of barca and its workers do not import it.

To update, download a published upstream wheel, verify its PyPI SHA-256, replace the complete
`packaging/` package and these three license files without formatting or editing them, update
this provenance, and run the doctor/version-warning controls plus a fresh wheel-only install.
Repository lint/format rules exclude this upstream directory to preserve its bytes.
