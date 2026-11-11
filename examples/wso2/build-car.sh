#!/usr/bin/env bash
# Build a small Carbon Application archive from the demo artifacts.
#
# A real CAR is a ZIP with synapse-configs/default/{api,sequences,endpoints}
# and, for API Manager, the apictl project metadata. RustO2 scans the archive
# for the artifact types it understands and reports unsupported ones.
set -euo pipefail
cd "$(dirname "$0")"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

mkdir -p \
  "$work/synapse-configs/default/api" \
  "$work/synapse-configs/default/sequences" \
  "$work/artifacts/PizzaShackAPI"

cp apim/pizzashack-api.yaml "$work/artifacts/PizzaShackAPI/api.yaml"
cp synapse/MediationDemo.xml "$work/synapse-configs/default/api/MediationDemo.xml"
cp synapse/CommonSequences.xml "$work/synapse-configs/default/sequences/CommonSequences.xml"

if command -v zip >/dev/null 2>&1; then
  (
    cd "$work"
    zip -qr "$OLDPWD/Rusto2Demo.car" .
  )
else
  python3 - "$work" "$(pwd)/Rusto2Demo.car" <<'PY'
import pathlib, sys, zipfile

root = pathlib.Path(sys.argv[1])
target = pathlib.Path(sys.argv[2])
with zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED) as archive:
    for path in sorted(root.rglob("*")):
        if path.is_file():
            archive.write(path, path.relative_to(root).as_posix())
PY
fi

echo "built $(pwd)/Rusto2Demo.car"
