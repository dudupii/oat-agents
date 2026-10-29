#!/usr/bin/env python3
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 oat-agents contributors
#
# The review eval's pass rule (evals/review/README.md): the reviewer must have caught
# the fixture's defect — at least one P1 finding citing the acceptance path's file and
# line. Exits 0 when caught, 1 when not, 2 when the input cannot be read as findings.

import json
import posixpath
import sys

# The fixture's acceptance path: the file whose signIn() the defect.patch changes.
ACCEPTANCE_PATH = "src/oidc.ts"


def filed_findings(document):
    """Both shapes this eval can be checked against: the JSON array a reviewer hands
    `dispatch done --findings-file`, and a work's findings record under the Run's state
    (every round's findings, flattened)."""
    if isinstance(document, list):
        return document
    if isinstance(document, dict) and isinstance(document.get("rounds"), list):
        return [f for r in document["rounds"] for f in r.get("findings", [])]
    return None


def cites_acceptance_path(finding):
    cited = posixpath.normpath(str(finding.get("file", "")).strip().lstrip("./"))
    return cited == ACCEPTANCE_PATH or cited.endswith("/" + ACCEPTANCE_PATH)


def has_line(finding):
    line = finding.get("line")
    return isinstance(line, int) and not isinstance(line, bool) and line > 0


def main(argv):
    if len(argv) != 2:
        print("usage: check.py FINDINGS_JSON", file=sys.stderr)
        return 2
    try:
        with open(argv[1], encoding="utf-8") as handle:
            document = json.load(handle)
    except (OSError, ValueError) as error:
        print("cannot read findings: {}".format(error), file=sys.stderr)
        return 2
    findings = filed_findings(document)
    if findings is None:
        print("expected a findings array or a work findings record", file=sys.stderr)
        return 2

    caught = [
        f
        for f in findings
        if f.get("severity") == "P1" and cites_acceptance_path(f) and has_line(f)
    ]
    if caught:
        for finding in caught:
            print(
                "P1 caught: {} at {}:{} — {}".format(
                    finding.get("id"), finding.get("file"), finding.get("line"),
                    finding.get("description"),
                )
            )
        print("review eval: PASS")
        return 0

    filed = ", ".join(sorted({str(f.get("severity")) for f in findings})) or "none"
    print(
        "review eval: FAIL — no P1 finding cites {} with a line (severities filed: {})".format(
            ACCEPTANCE_PATH, filed
        )
    )
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
