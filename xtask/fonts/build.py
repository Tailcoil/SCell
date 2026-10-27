# /// script
# requires-python = ">=3.12"
# dependencies = ["fonttools==4.67.0", "zstandard==0.25.0"]
# ///
"""合成内置字体 SCell Mono：Maple Mono 7.9 的拉丁可变字体 + 中文可变底稿 + Nerd Font 图标。

产物是一个 TTC，含正体与斜体两个可变字体（wght 100–800，默认 400）。两者的 glyf/loca/gvar/hmtx
完全相同：字形表依次放拉丁正体、拉丁斜体、中文、图标，每个字体只用自己的 cmap 与排版规则取用其中一部分，
中文与图标因此只存一份。斜体中的中文是正体字形。

中文底稿只有 wght 50 与 800 两个母版，合并后改用拉丁字体的 avar：先把中文的默认值移到 400，再缩放
增量使 400 与 700 两个字重与原字体一致，其余字重有细微偏差，400 以下与 400 相同。

用法：uv run xtask/fonts/build.py（由 cargo xtask fonts 调用）
"""

import copy
import hashlib
import io
import sys
import urllib.request
import zipfile
from pathlib import Path

import zstandard
from fontTools.feaLib.builder import addOpenTypeFeaturesFromString
from fontTools.misc.fixedTools import otRound
from fontTools.ttLib import TTCollection, TTFont, newTable
from fontTools.ttLib.tables import otTables
from fontTools.ttLib.tables._c_m_a_p import CmapSubtable
from fontTools.ttLib.tables._g_l_y_f import table__g_l_y_f
from fontTools.ttLib.tables._m_e_t_a import table__m_e_t_a
from fontTools.varLib import instancer
from fontTools.varLib.builder import buildVarData, buildVarRegionList, buildVarStore
from fontTools.varLib.models import normalizeValue, piecewiseLinearMap

COMMIT = "4d847729cbfc5b5d8cbae5795fd2348c70112648"  # v7.9
RAW = f"https://raw.githubusercontent.com/subframe7536/maple-font/{COMMIT}"
RELEASE = "https://github.com/subframe7536/maple-font/releases/download"
SOURCES = {
    "latin.zip": (
        f"{RELEASE}/v7.9/MapleMono-Variable.zip",
        "b9e57677cc4ec069ce178bd965d66cd14bd26c12e37d89d786ff959339bf8c93",
    ),
    "cn.zip": (
        f"{RELEASE}/cn-base/cn-base-variable.zip",
        "c7e670411d8df60b78c7755da64c5e0d190062a6f25589b0a5f7cd6f2c07c082",
    ),
    "nf.ttf": (
        f"{RAW}/source/MapleMono-NF-Base.ttf",
        "1b5ea8fd44d4ac66fdecd6979d707ef1e2cfac994a8cc02afe60927a1acebc16",
    ),
    "regular_cn.fea": (
        f"{RAW}/source/features/regular_cn.fea",
        "b49090665d91cd8641d154f9578a11f35f34c60e5386c66e6bb538d582c12977",
    ),
    "italic_cn.fea": (
        f"{RAW}/source/features/italic_cn.fea",
        "0cb7380d7a230317d2ca0112447a8622366859dd34b5cf033f7b654fab93c0cd",
    ),
    "OFL.txt": (
        f"{RAW}/OFL.txt",
        "eb2d28d2e565a0757e3d64e34ebb452e75a0cad87c0ab3faf4e08ba7596de902",
    ),
}

FAMILY = "SCell Mono"
# 中文按这两个字重对齐；终端只用常规与粗体
CN_EXACT = (400, 700)


def fetch(cache: Path, name: str) -> bytes:
    url, sha256 = SOURCES[name]
    path = cache / name
    if not path.exists():
        print(f"下载 {url}")
        with urllib.request.urlopen(url, timeout=300) as r:
            path.write_bytes(r.read())
    data = path.read_bytes()
    if hashlib.sha256(data).hexdigest() != sha256:
        path.unlink()
        sys.exit(f"{name} 校验失败，已删除缓存，请重试")
    return data


def from_zip(data: bytes, member: str) -> TTFont:
    with zipfile.ZipFile(io.BytesIO(data)) as z:
        return TTFont(io.BytesIO(z.read(member)))


def normalized(font: TTFont, weight: float) -> float:
    axis = next(a for a in font["fvar"].axes if a.axisTag == "wght")
    v = normalizeValue(weight, (axis.minValue, axis.defaultValue, axis.maxValue))
    if "avar" in font:
        v = piecewiseLinearMap(v, font["avar"].segments["wght"])
    return v


def rebase_cn(cn: TTFont, latin: TTFont) -> TTFont:
    """中文默认值移到 400、范围截到 400–800，再缩放增量使 CN_EXACT 两个字重在拉丁字体的 avar 下取值不变。"""
    lo, hi = CN_EXACT
    cn = instancer.instantiateVariableFont(cn, {"wght": (lo, lo, 800)})
    scale = normalized(cn, hi) / normalized(latin, hi)
    print(f"中文增量缩放 {scale:.4f}")
    for variations in cn["gvar"].variations.values():
        for v in variations:
            assert v.axes == {"wght": (0.0, 1.0, 1.0)}, v.axes
            v.coordinates = [
                None if c is None else (otRound(c[0] * scale), otRound(c[1] * scale))
                for c in v.coordinates
            ]
    return cn


class Source:
    def __init__(self, key: str, font: TTFont, skip_notdef: bool):
        self.key = key
        self.font = font
        self.names = font.getGlyphOrder()[1:] if skip_notdef else font.getGlyphOrder()


def face_names(sources: list[Source], italic: bool) -> dict[tuple[str, str], str]:
    """各来源字形在该字体中的名称。只影响排版规则的编译，最终的 post 表不存名称。"""
    latin = set(sources[0].names) | set(sources[1].names)
    names = {}
    for s in sources:
        for n in s.names:
            if s.key == "latin":
                name = f"{n}.up" if italic else n
            elif s.key == "italic":
                name = n if italic else f"{n}.it"
            else:
                # 与拉丁字体同名的以拉丁版本为准（官方合并也是如此），中文版改名保留
                name = f"{n}.{s.key}" if n in latin else n
            names[(s.key, n)] = name
    assert len(set(names.values())) == len(names), "字形名冲突"
    return names


def build_face(base: TTFont, sources: list[Source], italic: bool, fea: str) -> TTFont:
    names = face_names(sources, italic)
    order = [names[(s.key, n)] for s in sources for n in s.names]

    glyf = table__g_l_y_f()
    glyf.glyphOrder = order
    glyf.glyphs = {}
    metrics = {}
    variations = {}
    for s in sources:
        src_glyf = s.font["glyf"]
        src_gvar = s.font["gvar"].variations if "gvar" in s.font else {}
        for n in s.names:
            name = names[(s.key, n)]
            glyph = src_glyf[n]
            if glyph.isComposite():
                glyph = copy.deepcopy(glyph)
                for c in glyph.components:
                    c.glyphName = names[(s.key, c.glyphName)]
            glyf.glyphs[name] = glyph
            metrics[name] = s.font["hmtx"][n]
            variations[name] = src_gvar.get(n, [])

    font = base
    # 换字形顺序前先解析全部表：之后才解析的表会按新顺序读旧数据
    for tag in font.keys():
        if tag != "GlyphOrder":
            font[tag]
    font.setGlyphOrder(order)
    font["glyf"] = glyf
    font["loca"] = newTable("loca")
    hmtx = newTable("hmtx")
    hmtx.metrics = metrics
    font["hmtx"] = hmtx
    gvar = newTable("gvar")
    gvar.version, gvar.reserved, gvar.variations = 1, 0, variations
    font["gvar"] = gvar
    font["HVAR"] = zero_hvar(order)

    # 优先级：本字体的拉丁字形 > 中文 > 图标
    own = sources[1 if italic else 0]
    cmap = {}
    for s in reversed([own, sources[2], sources[3]]):
        for cp, n in s.font.getBestCmap().items():
            cmap[cp] = names[(s.key, n)]
    set_cmap(font, cmap)

    for tag in ("GSUB", "GPOS", "GDEF"):
        if tag in font:
            del font[tag]
    addOpenTypeFeaturesFromString(font, fea)

    post = font["post"]
    post.formatType = 3.0
    post.glyphOrder = None
    os2 = font["OS/2"]
    os2.recalcUnicodeRanges(font)
    # 拉丁、日文、简体中文、繁体中文，与官方 CN 版一致
    os2.ulCodePageRange1 = 1 << 0 | 1 << 17 | 1 << 18 | 1 << 20
    meta = table__m_e_t_a()
    meta.data = {"dlng": "Latn, Hans, Hant, Jpan", "slng": "Latn, Hans, Hant, Jpan"}
    font["meta"] = meta
    rename(font)
    return font


def zero_hvar(order: list[str]):
    """字宽不随字重变化：每个字形一行空增量（隐式映射，零个区域时每行 0 字节）。
    没有 HVAR 时渲染器要从 gvar 的幻影点推算字宽。"""
    rows = [[] for _ in order]
    store = buildVarStore(buildVarRegionList([], ["wght"]), [buildVarData([], rows, optimize=False)])
    hvar = otTables.HVAR()
    hvar.Version = 0x00010000
    hvar.VarStore = store
    hvar.AdvWidthMap = hvar.LsbMap = hvar.RsbMap = None
    table = newTable("HVAR")
    table.table = hvar
    return table


def set_cmap(font: TTFont, mapping: dict[int, str]):
    tables = []
    for platform, encoding, fmt, entries in (
        (0, 3, 4, {cp: n for cp, n in mapping.items() if cp <= 0xFFFF}),
        (3, 1, 4, {cp: n for cp, n in mapping.items() if cp <= 0xFFFF}),
        (0, 4, 12, mapping),
        (3, 10, 12, mapping),
    ):
        t = CmapSubtable.newSubtable(fmt)
        t.platformID, t.platEncID, t.language, t.cmap = platform, encoding, 0, entries
        tables.append(t)
    font["cmap"].tables = tables


def rename(font: TTFont):
    name = font["name"]
    for record in name.names:
        text = record.toUnicode()
        renamed = text.replace("Maple Mono", FAMILY).replace("MapleMono", FAMILY.replace(" ", ""))
        if record.nameID == 3:
            # 唯一标识：去掉 FontLab 的构建信息，免得与官方字体混淆
            renamed = renamed.split(";")[0] + f";{FAMILY.replace(' ', '')};Variable"
        if renamed != text:
            record.string = renamed
    name.setName(
        "SCell Mono is a modified version of Maple Mono 7.9 bundled with SCell: "
        "Latin, CJK and Nerd Font glyphs merged into one variable font.",
        10, 3, 1, 0x409,
    )


def main():
    root = Path(__file__).resolve().parents[2]
    cache = root / "target" / "fonts-cache"
    cache.mkdir(parents=True, exist_ok=True)
    out = root / "crates" / "scell-ui" / "assets" / "fonts"
    out.mkdir(parents=True, exist_ok=True)

    latin_zip = fetch(cache, "latin.zip")
    cn_zip = fetch(cache, "cn.zip")
    latin = from_zip(latin_zip, "MapleMono[wght].ttf")
    italic = from_zip(latin_zip, "MapleMono-Italic[wght].ttf")
    cn = rebase_cn(from_zip(cn_zip, "MapleMono-CN-VF.ttf"), latin)
    nf = TTFont(io.BytesIO(fetch(cache, "nf.ttf")))

    sources = [
        Source("latin", latin, skip_notdef=False),
        Source("italic", italic, skip_notdef=True),
        Source("cn", cn, skip_notdef=True),
        Source("nf", nf, skip_notdef=True),
    ]
    print(f"字形 {sum(len(s.names) for s in sources)}")

    faces = []
    for is_italic, base_zip_member, fea in (
        (False, "MapleMono[wght].ttf", "regular_cn.fea"),
        (True, "MapleMono-Italic[wght].ttf", "italic_cn.fea"),
    ):
        base = from_zip(latin_zip, base_zip_member)
        base.recalcTimestamp = False
        faces.append(build_face(base, sources, is_italic, fetch(cache, fea).decode()))

    ttc = TTCollection()
    ttc.fonts = faces
    buf = io.BytesIO()
    ttc.save(buf, shareTables=True)
    raw = buf.getvalue()
    # 未压缩的副本留在缓存里，供检查与渲染预览
    (cache / "SCellMono.ttc").write_bytes(raw)

    params = zstandard.ZstdCompressionParameters.from_level(
        19, window_log=27, enable_ldm=True, write_content_size=True
    )
    packed = zstandard.ZstdCompressor(compression_params=params).compress(raw)
    (out / "SCellMono.ttc.zst").write_bytes(packed)
    (out / "OFL.txt").write_bytes(fetch(cache, "OFL.txt"))
    print(f"TTC {len(raw) / 1e6:.2f} MB，压缩后 {len(packed) / 1e6:.2f} MB")
    print(f"sha256 {hashlib.sha256(raw).hexdigest()}")


if __name__ == "__main__":
    main()
