#!/usr/bin/env python3
"""Generates crates/jellyfin-api/src/dto.rs from Jellyfin's OpenAPI spec.

Only the schemas and fields jellyroll uses are generated (see SELECT), so
the crate stays small and compiles fast; requests ask the server for the
same minimal field sets (spec 8: `Fields=`). Every field is an `Option`
and every struct defaults missing fields, and every enum has a catch-all
variant, so a newer or older server can never make parsing panic (spec 12).

Usage:
    scripts/gen-jellyfin-dto.py                 # pinned spec (downloaded, sha256-checked)
    scripts/gen-jellyfin-dto.py openapi.json    # a local spec, e.g. from SERVER/api-docs/openapi.json

The pinned spec is the one the Jellyfin TypeScript SDK checks in for the
current stable server; see docs/VERSIONS.md.
"""

import hashlib
import json
import pathlib
import re
import subprocess
import sys
import urllib.request

SDK_COMMIT = "7685bb8e2c3aeda89ca831c2228923affd022fbb"
SPEC_URL = (
    "https://media.githubusercontent.com/media/jellyfin/jellyfin-sdk-typescript/"
    f"{SDK_COMMIT}/openapi.json"
)
SPEC_SHA256 = "bc8c668b38b65e48ac6ec8c6f763b4bce36b955a0d4a58a27198ee4ec636ab80"

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "crates/jellyfin-api/src/dto.rs"

# Schema -> fields to generate. "Schema.Field" keys select fields of an
# inline object property. Enums referenced by selected fields are generated
# whole.
SELECT = {
    "PublicSystemInfo": ["ServerName", "Version", "ProductName", "Id", "LocalAddress",
                         "StartupWizardCompleted"],
    "QuickConnectResult": ["Authenticated", "Secret", "Code"],
    "AuthenticationResult": ["User", "AccessToken", "ServerId"],
    "UserDto": ["Name", "Id", "ServerId", "PrimaryImageTag", "HasPassword"],
    "BaseItemDtoQueryResult": ["Items", "TotalRecordCount", "StartIndex"],
    "BaseItemDto": [
        "Id", "Name", "Type", "MediaType", "CollectionType", "IsFolder",
        "SeriesName", "SeriesId", "ParentIndexNumber", "IndexNumber",
        "ProductionYear", "RunTimeTicks", "OfficialRating", "CommunityRating",
        "Overview", "PrimaryImageAspectRatio", "ImageTags", "BackdropImageTags",
        "ImageBlurHashes", "ParentBackdropItemId", "ParentBackdropImageTags",
        "SeriesPrimaryImageTag", "ParentThumbItemId", "ParentThumbImageTag",
        "UserData",
        # M6: detail pages, library grids.
        "Genres", "Taglines", "People", "MediaStreams", "ChildCount", "SeasonId",
        "SeasonName", "ParentId", "PremiereDate", "Status", "CriticRating", "SortName",
        "DateCreated", "HasSubtitles", "Container",
        # M7: player (chapter markers, trickplay seek previews).
        "Chapters", "Trickplay",
        # Item page track picker: the server's per-user default tracks.
        "MediaSources",
    ],
    "BaseItemPerson": ["Name", "Id", "Role", "Type", "PrimaryImageTag"],
    "MediaStream": ["Type", "Codec", "Language", "DisplayTitle", "Title", "Profile",
                    "BitDepth", "Width", "Height", "Channels", "ChannelLayout",
                    "VideoRangeType", "AudioSpatialFormat", "IsDefault", "IsExternal",
                    "Index",
                    # M7: track menus, external subtitles.
                    "IsForced", "IsHearingImpaired", "DeliveryMethod", "DeliveryUrl",
                    "IsTextSubtitleStream", "SupportsExternalStream",
                    # M8: refresh-rate matching.
                    "RealFrameRate", "AverageFrameRate"],
    # M7: player.
    "ChapterInfo": ["StartPositionTicks", "Name"],
    "TrickplayInfoDto": ["Width", "Height", "TileWidth", "TileHeight", "ThumbnailCount",
                         "Interval", "Bandwidth"],
    "PlaybackInfoResponse": ["MediaSources", "PlaySessionId", "ErrorCode"],
    "MediaSourceInfo": ["Id", "Protocol", "Container", "Size", "Bitrate", "RunTimeTicks",
                        "SupportsDirectPlay", "SupportsDirectStream", "SupportsTranscoding",
                        "TranscodingUrl", "MediaStreams", "DefaultAudioStreamIndex",
                        "DefaultSubtitleStreamIndex"],
    "MediaSegmentDto": ["Id", "ItemId", "Type", "StartTicks", "EndTicks"],
    "MediaSegmentDtoQueryResult": ["Items", "TotalRecordCount"],
    "BaseItemDto.ImageBlurHashes": ["Primary", "Backdrop", "Thumb", "Logo"],
    "UserItemDataDto": ["PlayedPercentage", "UnplayedItemCount", "PlaybackPositionTicks",
                        "PlayCount", "IsFavorite", "Played", "ItemId"],
    # Live updates and remote control over the WebSocket (spec 8).
    "LibraryUpdateInfo": ["FoldersAddedTo", "FoldersRemovedFrom", "ItemsAdded",
                          "ItemsRemoved", "ItemsUpdated", "CollectionFolders"],
    "UserDataChangeInfo": ["UserId", "UserDataList"],
    "PlaystateRequest": ["Command", "SeekPositionTicks", "ControllingUserId"],
    "GeneralCommand": ["Name", "ControllingUserId", "Arguments"],
    "PlayRequest": ["ItemIds", "StartPositionTicks", "PlayCommand", "MediaSourceId",
                    "AudioStreamIndex", "SubtitleStreamIndex", "StartIndex"],
    "ClientCapabilitiesDto": ["PlayableMediaTypes", "SupportedCommands",
                              "SupportsMediaControl", "SupportsPersistentIdentifier"],
}


def load_spec(argv):
    if len(argv) > 1:
        data = pathlib.Path(argv[1]).read_bytes()
        source = argv[1]
    else:
        with urllib.request.urlopen(SPEC_URL, timeout=60) as r:
            data = r.read()
        digest = hashlib.sha256(data).hexdigest()
        if digest != SPEC_SHA256:
            sys.exit(f"spec sha256 mismatch: got {digest}, want {SPEC_SHA256}")
        source = f"jellyfin-sdk-typescript@{SDK_COMMIT[:12]}"
    return json.loads(data), source, hashlib.sha256(data).hexdigest()


def snake(name):
    s = re.sub(r"(?<=[a-z0-9])([A-Z])", r"_\1", name)
    s = re.sub(r"(?<=[A-Z])([A-Z][a-z])", r"_\1", s)
    s = s.lower()
    return "r#type" if s == "type" else s


def variant(value):
    parts = re.split(r"[^A-Za-z0-9]+", value)
    v = "".join(p[:1].upper() + p[1:] for p in parts if p)
    if not v or v[0].isdigit():
        v = "V" + v
    return v


class Gen:
    def __init__(self, spec):
        self.schemas = spec["components"]["schemas"]
        self.enums = {}
        self.out = []

    def ref_name(self, prop):
        ref = prop.get("$ref")
        if not ref and prop.get("allOf"):
            ref = prop["allOf"][0].get("$ref")
        return ref.split("/")[-1] if ref else None

    def rust_type(self, owner, field, prop):
        ref = self.ref_name(prop)
        if ref:
            target = self.schemas[ref]
            if "enum" in target:
                self.enums[ref] = target["enum"]
                return ref
            if ref not in SELECT:
                sys.exit(f"{owner}.{field} references {ref}, which is not in SELECT")
            return ref
        t = prop.get("type")
        fmt = prop.get("format", "")
        if t == "string":
            return "String"
        if t == "integer":
            return "i64" if fmt == "int64" else "i32"
        if t == "number":
            return "f32" if fmt == "float" else "f64"
        if t == "boolean":
            return "bool"
        if t == "array":
            return f"Vec<{self.rust_type(owner, field, prop['items'])}>"
        if t == "object":
            if "properties" in prop:
                key = f"{owner}.{field}"
                if key not in SELECT:
                    sys.exit(f"{key} is an inline object; add it to SELECT")
                return field
            inner = prop.get("additionalProperties") or {"type": "string"}
            v = self.rust_type(owner, field, inner)
            if inner.get("nullable"):
                v = f"Option<{v}>"
            return f"HashMap<String, {v}>"
        sys.exit(f"{owner}.{field}: unsupported type {prop}")

    def struct(self, name, props, fields, doc):
        o = self.out
        if doc:
            for line in doc.strip().splitlines():
                o.append(f"/// {line}".rstrip())
        o.append("#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]")
        o.append("#[serde(default)]")
        o.append(f"pub struct {name} {{")
        for f in fields:
            if f not in props:
                sys.exit(f"{name}.{f} not in the spec")
            t = self.rust_type(name.split(".")[0], f, props[f])
            o.append(f'    #[serde(rename = "{f}", skip_serializing_if = "Option::is_none")]')
            o.append(f"    pub {snake(f)}: Option<{t}>,")
        o.append("}")
        o.append("")

    def run(self):
        for key, fields in SELECT.items():
            if "." in key:
                owner, field = key.split(".")
                prop = self.schemas[owner]["properties"][field]
                self.struct(field, prop["properties"], fields, prop.get("description"))
            else:
                s = self.schemas[key]
                self.struct(key, s["properties"], fields, s.get("description"))
        for name in sorted(self.enums):
            o = self.out
            doc = self.schemas[name].get("description")
            if doc:
                for line in doc.strip().splitlines():
                    o.append(f"/// {line}".rstrip())
            o.append("#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]")
            o.append(f"pub enum {name} {{")
            for v in self.enums[name]:
                o.append(f'    #[serde(rename = "{v}")]')
                o.append(f"    {variant(v)},")
            o.append("    /// A value this build doesn't know (newer server).")
            o.append("    #[default]")
            o.append("    #[serde(other)]")
            o.append("    Unrecognized,")
            o.append("}")
            o.append("")


def main():
    spec, source, digest = load_spec(sys.argv)
    g = Gen(spec)
    g.run()
    header = [
        "// @generated by scripts/gen-jellyfin-dto.py. Do not edit by hand.",
        f"// Source: {source}",
        f"// Jellyfin API {spec['info']['version']}, OpenAPI {spec['openapi']}",
        f"// Spec sha256: {digest}",
        "",
        "use std::collections::HashMap;",
        "",
        "use serde::{Deserialize, Serialize};",
        "",
    ]
    OUT.write_text("\n".join(header + g.out))
    subprocess.run(["rustfmt", "--edition", "2024", str(OUT)], check=True)
    print(f"wrote {OUT.relative_to(ROOT)}: {len(SELECT)} structs, {len(g.enums)} enums")


if __name__ == "__main__":
    main()
