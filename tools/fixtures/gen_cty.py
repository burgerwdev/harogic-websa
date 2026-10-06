#!/usr/bin/env python3
"""Regenerate frontend/src/sdr/ctyData.ts from the AD1C cty.dat country file.

The source (tools/fixtures/cty.dat) is the DXCC prefix→entity table published at
https://www.country-files.com/cty/cty.dat . This script folds it into a compact
TypeScript module that the FT8 decode window uses to turn a callsign prefix into a
country/region name, with a curated Chinese name for the entities an operator sees
most often (anything else falls back to the English DXCC name).

Usage:  python3 tools/fixtures/gen_cty.py   (writes frontend/src/sdr/ctyData.ts)
"""

import json
import pathlib

ROOT = pathlib.Path(__file__).resolve().parent.parent.parent
CTY_DAT = ROOT / "tools" / "fixtures" / "cty.dat"
OUT = ROOT / "frontend" / "src" / "sdr" / "ctyData.ts"

# English entity name (exactly as cty.dat spells it) → Chinese display name.
ZH_NAMES = {
    "Vietnam": "越南",
    "Sri Lanka": "斯里兰卡",
    "Israel": "以色列",
    "Cyprus": "塞浦路斯",
    "Croatia": "克罗地亚",
    "Malta": "马耳他",
    "Kuwait": "科威特",
    "West Malaysia": "马来西亚",
    "East Malaysia": "马来西亚",
    "Singapore": "新加坡",
    "Oman": "阿曼",
    "Qatar": "卡塔尔",
    "Bahrain": "巴林",
    "Taiwan": "台湾",
    "China": "中国",
    "Chile": "智利",
    "Portugal": "葡萄牙",
    "Fed. Rep. of Germany": "德国",
    "Philippines": "菲律宾",
    "Spain": "西班牙",
    "Ireland": "爱尔兰",
    "Estonia": "爱沙尼亚",
    "France": "法国",
    "England": "英格兰",
    "Northern Ireland": "北爱尔兰",
    "Hungary": "匈牙利",
    "Switzerland": "瑞士",
    "Republic of Korea": "韩国",
    "Thailand": "泰国",
    "Saudi Arabia": "沙特阿拉伯",
    "Italy": "意大利",
    "African Italy": "意大利",
    "Japan": "日本",
    "Jordan": "约旦",
    "United States": "美国",
    "Norway": "挪威",
    "Argentina": "阿根廷",
    "Luxembourg": "卢森堡",
    "Lithuania": "立陶宛",
    "Bulgaria": "保加利亚",
    "Lebanon": "黎巴嫩",
    "Austria": "奥地利",
    "Finland": "芬兰",
    "Czech Republic": "捷克",
    "Belgium": "比利时",
    "Denmark": "丹麦",
    "DPR of Korea": "朝鲜",
    "Netherlands": "荷兰",
    "Brazil": "巴西",
    "Slovenia": "斯洛文尼亚",
    "Sweden": "瑞典",
    "Poland": "波兰",
    "Greece": "希腊",
    "Asiatic Turkey": "土耳其",
    "European Turkey": "土耳其",
    "Iceland": "冰岛",
    "European Russia": "俄罗斯",
    "Asiatic Russia": "俄罗斯",
    "Ukraine": "乌克兰",
    "Canada": "加拿大",
    "Australia": "澳大利亚",
    "Hong Kong": "香港",
    "India": "印度",
    "Mexico": "墨西哥",
    "Macao": "澳门",
    "Indonesia": "印度尼西亚",
    "Latvia": "拉脱维亚",
    "Romania": "罗马尼亚",
    "Serbia": "塞尔维亚",
    "New Zealand": "新西兰",
    "South Africa": "南非",
    "Albania": "阿尔巴尼亚",
    "Algeria": "阿尔及利亚",
    "Angola": "安哥拉",
    "Armenia": "亚美尼亚",
    "Azerbaijan": "阿塞拜疆",
    "Bangladesh": "孟加拉国",
    "Belarus": "白俄罗斯",
    "Belize": "伯利兹",
    "Benin": "贝宁",
    "Bhutan": "不丹",
    "Bolivia": "玻利维亚",
    "Botswana": "博茨瓦纳",
    "Brunei Darussalam": "文莱",
    "Burkina Faso": "布基纳法索",
    "Burundi": "布隆迪",
    "Cabo Verde": "佛得角",
    "Cambodia": "柬埔寨",
    "Cameroon": "喀麦隆",
    "Central African Republic": "中非",
    "Chad": "乍得",
    "Cote d'Ivoire": "科特迪瓦",
    "Cuba": "古巴",
    "Dem. Rep. of the Congo": "刚果（金）",
    "Djibouti": "吉布提",
    "Dominican Republic": "多米尼加",
    "Ecuador": "厄瓜多尔",
    "Egypt": "埃及",
    "El Salvador": "萨尔瓦多",
    "Equatorial Guinea": "赤道几内亚",
    "Eritrea": "厄立特里亚",
    "Ethiopia": "埃塞俄比亚",
    "Fiji": "斐济",
    "Gabon": "加蓬",
    "Georgia": "格鲁吉亚",
    "Ghana": "加纳",
    "Guatemala": "危地马拉",
    "Guinea": "几内亚",
    "Guinea-Bissau": "几内亚比绍",
    "Guyana": "圭亚那",
    "Haiti": "海地",
    "Honduras": "洪都拉斯",
    "Iran": "伊朗",
    "Iraq": "伊拉克",
    "Jamaica": "牙买加",
    "Kazakhstan": "哈萨克斯坦",
    "Kenya": "肯尼亚",
    "Kingdom of Eswatini": "斯威士兰",
    "Kyrgyzstan": "吉尔吉斯斯坦",
    "Laos": "老挝",
    "Lesotho": "莱索托",
    "Liberia": "利比里亚",
    "Libya": "利比亚",
    "Madagascar": "马达加斯加",
    "Malawi": "马拉维",
    "Mali": "马里",
    "Mauritania": "毛里塔尼亚",
    "Mauritius": "毛里求斯",
    "Moldova": "摩尔多瓦",
    "Mongolia": "蒙古",
    "Montenegro": "黑山",
    "Morocco": "摩洛哥",
    "Mozambique": "莫桑比克",
    "Myanmar": "缅甸",
    "Namibia": "纳米比亚",
    "Nepal": "尼泊尔",
    "New Caledonia": "新喀里多尼亚",
    "Nicaragua": "尼加拉瓜",
    "Niger": "尼日尔",
    "Nigeria": "尼日利亚",
    "North Macedonia": "北马其顿",
    "Pakistan": "巴基斯坦",
    "Panama": "巴拿马",
    "Papua New Guinea": "巴布亚新几内亚",
    "Paraguay": "巴拉圭",
    "Peru": "秘鲁",
    "Republic of Kosovo": "科索沃",
    "Republic of South Sudan": "南苏丹",
    "Republic of the Congo": "刚果（布）",
    "Rwanda": "卢旺达",
    "Samoa": "萨摩亚",
    "Senegal": "塞内加尔",
    "Seychelles": "塞舌尔",
    "Sierra Leone": "塞拉利昂",
    "Slovak Republic": "斯洛伐克",
    "Solomon Islands": "所罗门群岛",
    "Somalia": "索马里",
    "Sudan": "苏丹",
    "Suriname": "苏里南",
    "Syria": "叙利亚",
    "Tajikistan": "塔吉克斯坦",
    "Tanzania": "坦桑尼亚",
    "The Gambia": "冈比亚",
    "Timor - Leste": "东帝汶",
    "Togo": "多哥",
    "Tonga": "汤加",
    "Tunisia": "突尼斯",
    "Turkmenistan": "土库曼斯坦",
    "Uganda": "乌干达",
    "United Arab Emirates": "阿联酋",
    "Uruguay": "乌拉圭",
    "Uzbekistan": "乌兹别克斯坦",
    "Vanuatu": "瓦努阿图",
    "Vatican City": "梵蒂冈",
    "Venezuela": "委内瑞拉",
    "Yemen": "也门",
    "Zambia": "赞比亚",
    "Zimbabwe": "津巴布韦",
}


def parse_entities():
    text = CTY_DAT.read_text(encoding="utf-8", errors="replace").replace("\r\n", "\n")
    lines = text.split("\n")
    entities = []  # list of {"name", "prefixes", "exact"}
    i, n = 0, len(lines)
    while i < n:
        line = lines[i]
        if not line.strip() or line[0] in " \t":
            i += 1
            continue
        # Header: `Name: cq: itu: cont: lat: lon: tz: prefix:`
        parts = line.split(":")
        name = parts[0].strip()
        primary = parts[-2].strip()
        aliases = [primary]
        i += 1
        # Aliases continue on indented lines until the next header.
        while i < n and (not lines[i].strip() or lines[i][0] in " \t"):
            if lines[i].strip():
                aliases.extend(lines[i].split(","))
            i += 1
        prefixes, exact = [], []
        for alias in aliases:
            alias = alias.strip().rstrip(";").strip()
            if not alias:
                continue
            if alias.startswith("="):
                exact.append(alias[1:].lower())
                continue
            alias = alias.lstrip("*")
            # Drop CQ/ITU zone annotations: `3H0(23)[42]` → `3H0`.
            alias = alias.split("(")[0].split("[")[0].strip()
            if alias:
                prefixes.append(alias.upper())
        entities.append({"name": name, "prefixes": prefixes, "exact": exact})
    return entities


def build_module(entities):
    names = [e["name"] for e in entities]
    zh = {}
    for idx, name in enumerate(names):
        if name in ZH_NAMES:
            zh[idx] = ZH_NAMES[name]

    prefixes = {}
    exact = {}
    for idx, e in enumerate(entities):
        for p in e["prefixes"]:
            prefixes.setdefault(p, idx)
        for c in e["exact"]:
            exact.setdefault(c, idx)

    body = [
        "// Generated from tools/fixtures/cty.dat (AD1C country files) by tools/fixtures/gen_cty.py — do not edit.",
        "// Regenerate with:  python3 tools/fixtures/gen_cty.py",
        "",
        "/** DXCC entity names (English, from cty.dat), indexed by the other tables. */",
        f"export const CTY_ENTITIES: readonly string[] = {json.dumps(names, ensure_ascii=False)};",
        "",
        "/** Entity index → Chinese display name (entities without an entry fall back to English). */",
        f"export const CTY_ZH: Readonly<Record<number, string>> = {json.dumps(zh, ensure_ascii=False)};",
        "",
        "/** Callsign prefix (uppercase) → entity index. */",
        f"export const CTY_PREFIXES: Readonly<Record<string, number>> = {json.dumps(prefixes, ensure_ascii=False)};",
        "",
        "/** Exact callsign override (lowercase, cty.dat `=CALL` entries) → entity index. */",
        f"export const CTY_EXACT: Readonly<Record<string, number>> = {json.dumps(exact, ensure_ascii=False)};",
        "",
    ]
    return "\n".join(body)


def main():
    entities = parse_entities()
    if not entities:
        raise SystemExit("cty.dat parsed to zero entities — aborting")
    module = build_module(entities)
    OUT.write_text(module, encoding="utf-8")
    print(f"wrote {OUT} ({len(entities)} entities, "
          f"{sum(len(e['prefixes']) for e in entities)} prefixes, "
          f"{sum(len(e['exact']) for e in entities)} exact)")


if __name__ == "__main__":
    main()
