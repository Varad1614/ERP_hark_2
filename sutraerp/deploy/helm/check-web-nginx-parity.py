#!/usr/bin/env python3
"""Part 2 · S5-deploy: the chart's web nginx config must carry every location of the image config.

deploy/docker/nginx.conf (baked into the web image, used by compose / the on-prem bundle) and the
chart's web ConfigMap (templates/web/configmap.yaml, rendered) are two copies of one site config.
Stage 4 found the chart missing the 11 MiB document-upload location the image had, so uploads above
2 MiB answered 413 on Kubernetes only. This check fails when any `location` of nginx.conf is absent
from the rendered ConfigMap, or when a location's request-size / timeout directives differ.

Usage: check-web-nginx-parity.py deploy/docker/nginx.conf <rendered.yaml>...
"""
import re
import sys

LIMIT_DIRECTIVES = ("client_max_body_size", "client_body_timeout", "proxy_read_timeout",
                    "proxy_send_timeout", "limit_req")


def locations(text):
    """{location selector: {directive: value}} for every `location … {` block (one level)."""
    out = {}
    for m in re.finditer(r"^\s*location\s+([^{]+?)\s*\{", text, re.M):
        sel = " ".join(m.group(1).split())
        depth, i = 1, m.end()
        while depth and i < len(text):
            depth += {"{": 1, "}": -1}.get(text[i], 0)
            i += 1
        body = text[m.end():i - 1]
        d = {}
        for line in body.splitlines():
            line = line.split("#", 1)[0].strip().rstrip(";")
            if not line:
                continue
            key, _, val = line.partition(" ")
            if key in LIMIT_DIRECTIVES:
                d[key] = " ".join(val.split())
        out[sel] = d
    return out


def main(argv):
    if len(argv) < 3:
        print(__doc__, file=sys.stderr)
        return 2
    image = locations(open(argv[1]).read())
    if not image:
        print(f"FAIL no locations parsed from {argv[1]}", file=sys.stderr)
        return 1
    bad = 0
    for path in argv[2:]:
        text = open(path).read()
        if "kind: ConfigMap" not in text or "sutra_csp_script_hashes" not in text:
            print(f"FAIL {path}: no rendered web nginx ConfigMap", file=sys.stderr)
            bad += 1
            continue
        chart = locations(text)
        for sel, want in image.items():
            got = chart.get(sel)
            if got is None:
                print(f"FAIL {path}: chart web config lacks `location {sel}` (image nginx.conf has it)",
                      file=sys.stderr)
                bad += 1
            elif got != want:
                print(f"FAIL {path}: `location {sel}` differs: image {want} vs chart {got}",
                      file=sys.stderr)
                bad += 1
        if not bad:
            print(f"ok {path}: {len(image)} image locations present in the chart with the same limits")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
