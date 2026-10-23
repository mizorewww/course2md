#!/usr/bin/env python3
"""Controlled extractor for native UX acceptance, enabled only in the isolated test app.

Only explicit BV1UX* test sources are simulated. Ordinary URLs use installed yt-dlp.
The GUI, subtitle parser, ffmpeg, worker, storage and export code remain real.
"""
import json
import os
from pathlib import Path
import shutil
import sys
import time
from urllib.parse import urlsplit, parse_qs

from ux_common import fixture_root, real_tool


args = sys.argv[1:]
source = next((arg for arg in args if "bilibili.com/video/BV1UX" in arg), None)
if source is None:
    binary = real_tool("yt-dlp")
    if binary is None:
        sys.exit("Real yt-dlp is unavailable")
    os.execv(binary, [binary, *args])

root = fixture_root()
case = urlsplit(source).path.rsplit("/", 1)[-1]
part = parse_qs(urlsplit(source).query).get("p", [None])[0]
with (root / "extractor-requests.jsonl").open("a") as log:
    log.write(json.dumps({"time": time.time(), "source": source, "subtitles": "--write-subs" in args, "simulate": "--simulate" in args}) + "\n")

if "SLOW" in case:
    time.sleep(25)
# Restore captions for the same synthetic source without replacing its draft.
recovery_file = root / "subtitle-recovery.json"
recovered = json.loads(recovery_file.read_text()) if recovery_file.exists() else []
if "--write-subs" in args and ("TIMEOUT" in case or "LOGIN" in case) and case not in recovered:
    print("ERROR: Sign in to read these subtitles" if "LOGIN" in case else "ERROR: timed out while reading subtitle metadata", file=sys.stderr)
    sys.exit(1)
if "COLLECTION" in case and part is None:
    print(json.dumps({"_type": "playlist", "id": case, "title": "验收合辑：两节课程", "extractor": "BiliBili", "entries": [{"_type": "url", "url": source + "?p=" + str(i), "ie_key": "BiliBili"} for i in [1, 2]] + [None]}, ensure_ascii=False))
    sys.exit(0)

def cues(text):
    return "1\n00:00:00,000 --> 00:00:07,000\n" + text + "\n\n2\n00:00:07,000 --> 00:00:17,000\n" + text + "\n"

subtitles = {} if "NONE" in case else {
    "fr": [{"ext": "srt", "data": cues("Le risque et le rendement sont liés.")}],
    "ja": [{"ext": "srt", "data": cues("リスクとリターンには関係があります。")}],
    "zh-Hans": [{"ext": "srt", "data": cues("人工字幕：分散投资可以减少个别资产带来的影响。")}],
    "ai-zh": [{"ext": "srt", "data": cues("自动字幕：风险与收益需要一起判断。")}],
}
meta = {"id": case + ("_p" + part if part else ""), "title": "验收课程" + (" · 第 " + part + " 节" if part else " · " + case), "uploader": "合成测试讲师", "duration": 18, "extractor": "BiliBili", "webpage_url": source, "language": "ja", "subtitles": subtitles, "automatic_captions": {}}
if "--dump-single-json" in args or "--dump-json" in args or "-J" in args or "-j" in args:
    print(json.dumps(meta, ensure_ascii=False))
    sys.exit(0)
if "-o" in args:
    output = Path(args[args.index("-o") + 1])
    output.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(root / "media/lecture-a.mp4", output)
    print("[download] Synthetic acceptance media copied", file=sys.stderr)
    sys.exit(0)
sys.exit("Unsupported acceptance extractor invocation")
