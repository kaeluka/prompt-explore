#!/usr/bin/env python3
"""Render openapi.json to API.md (human-readable). No dependencies.

Input is authoritative; this is a pure projection. Regenerate via
scripts/dump-openapi.sh.
"""
import json
import sys

def anchor(name):
    return name.lower()

def esc(text):
    return str(text).replace("|", "\\|").replace("\n", " ").strip()

def type_str(s, schemas):
    if not s:
        return "any"
    if "$ref" in s:
        name = s["$ref"].split("/")[-1]
        return f"[`{name}`](#{anchor(name)})"
    if "oneOf" in s:
        variants = s["oneOf"]
        non_null = [v for v in variants if v.get("type") != "null"]
        nullable = len(non_null) < len(variants)
        if len(non_null) == 1:
            return type_str(non_null[0], schemas) + ("?" if nullable else "")
        joined = " or ".join(type_str(v, schemas) for v in non_null)
        return joined + (" (nullable)" if nullable else "")
    if "enum" in s:
        return " \\| ".join(f"`{v}`" for v in s["enum"])
    t = s.get("type")
    if isinstance(t, list):
        non_null = [x for x in t if x != "null"]
        base = non_null[0] if non_null else "null"
        return base + ("?" if len(non_null) < len(t) else "")
    if t == "array":
        return type_str(s.get("items", {}), schemas) + "[]"
    if t == "object":
        ap = s.get("additionalProperties")
        if ap:
            return f"map&lt;string, {type_str(ap, schemas)}&gt;"
        return "object" if "properties" in s else "map&lt;string, any&gt;"
    return t or "any"

def field_table(schema, schemas):
    props = schema.get("properties", {})
    required = set(schema.get("required", []))
    if not props:
        return ""
    rows = ["| Field | Type | Required | Description |",
            "|---|---|---|---|"]
    for name in sorted(props):
        p = props[name]
        rows.append(
            f"| `{name}` | {type_str(p, schemas)} | "
            f"{'yes' if name in required else 'no'} | {esc(p.get('description', ''))} |"
        )
    return "\n".join(rows)

def variant_name(variant):
    """For tagged enums, the variant name is the single-value enum on its tag property."""
    for prop in variant.get("properties", {}).values():
        if len(prop.get("enum", [])) == 1:
            return prop["enum"][0]
    return None

def render_schema(name, schema, schemas):
    out = [f"### `{name}`", ""]
    if schema.get("description"):
        out += [esc(schema["description"]), ""]
    if "enum" in schema:
        out.append("Values: " + ", ".join(f"`{v}`" for v in schema["enum"]))
        out.append("")
    elif "oneOf" in schema:
        for v in schema["oneOf"]:
            vn = variant_name(v)
            out.append(f"**Variant `{vn}`**" if vn else "**Variant**")
            out.append("")
            tbl = field_table(v, schemas)
            out.append(tbl if tbl else f"`{type_str(v, schemas)}`")
            out.append("")
    else:
        tbl = field_table(schema, schemas)
        if tbl:
            out += [tbl, ""]
        else:
            out += [f"Type: {type_str(schema, schemas)}", ""]
    return "\n".join(out)

def body_md(schema, schemas):
    if not schema:
        return ""
    if "$ref" in schema:
        name = schema["$ref"].split("/")[-1]
        tbl = field_table(schemas.get(name, {}), schemas)
        out = f"Body: [`{name}`](#{anchor(name)})\n"
        if tbl:
            out += "\n" + tbl + "\n"
        return out
    return f"Body: {type_str(schema, schemas)}\n"

def main():
    spec = json.load(open(sys.argv[1] if len(sys.argv) > 1 else "openapi.json"))
    schemas = spec.get("components", {}).get("schemas", {})
    info = spec["info"]

    md = [f"# {info['title']}", ""]
    if info.get("description"):
        md += [esc(info["description"]), ""]
    md += [f"Version: `{info.get('version', '?')}` — generated from `openapi.json`; "
           f"do not edit by hand (see `scripts/dump-openapi.sh`).", ""]

    md += ["## Endpoints", ""]
    for path in sorted(spec["paths"]):
        for method, op in sorted(spec["paths"][path].items()):
            md.append(f"### `{method.upper()} {path}`")
            md.append("")
            if op.get("description") or op.get("summary"):
                md += [esc(op.get("description") or op["summary"]), ""]
            if op.get("parameters"):
                md += ["| Parameter | In | Type | Description |", "|---|---|---|---|"]
                for p in op["parameters"]:
                    md.append(
                        f"| `{p['name']}` | {p.get('in','')} | "
                        f"{type_str(p.get('schema', {}), schemas)} | {esc(p.get('description',''))} |"
                    )
                md.append("")
            rb = op.get("requestBody", {}).get("content", {}).get("application/json", {})
            if rb.get("schema"):
                md += [body_md(rb["schema"], schemas), ""]
            md += ["| Status | Response |", "|---|---|"]
            for status, resp in sorted(op.get("responses", {}).items()):
                desc = esc(resp.get("description", ""))
                content = resp.get("content", {}).get("application/json", {})
                if content.get("schema"):
                    desc += (": " if desc else "") + type_str(content["schema"], schemas)
                elif "text/html" in resp.get("content", {}) and "(HTML)" not in desc:
                    desc += (" " if desc else "") + "(HTML)"
                md.append(f"| `{status}` | {desc} |")
            md.append("")

    md += ["## Schemas", ""]
    for name in sorted(schemas):
        md.append(render_schema(name, schemas[name], schemas))

    out_path = sys.argv[2] if len(sys.argv) > 2 else "API.md"
    with open(out_path, "w") as f:
        f.write("\n".join(md).rstrip() + "\n")
    print(f"wrote {out_path} ({len(spec['paths'])} endpoints, {len(schemas)} schemas)")

if __name__ == "__main__":
    main()
