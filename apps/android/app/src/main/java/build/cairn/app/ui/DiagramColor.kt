package build.cairn.app.ui

import kotlin.math.abs
import kotlin.math.roundToInt

private val CSS_NUMBER = Regex("[-+]?(?:[0-9]+(?:\\.[0-9]*)?|\\.[0-9]+)(?:[eE][-+]?[0-9]+)?%?")

/** A CSS color the parser accepted, or null when CSS would reject it too. */
internal fun cssColor(value: String, ink: Int): Int? {
    val color = value.lowercase()
    if (color.startsWith("#")) {
        val hex = color.substring(1)
        if (hex.any { Character.digit(it, 16) < 0 }) return null
        val full =
            when (hex.length) {
                3,
                4 -> hex.map { "$it$it" }.joinToString("")
                6,
                8 -> hex
                else -> return null
            }
        val rgb = full.substring(0, 6).toLong(16).toInt()
        val alpha = if (full.length == 8) full.substring(6).toInt(16) else 255
        return (alpha shl 24) or rgb
    }

    val open = color.indexOf('(')
    if (open > 0 && color.endsWith(")")) {
        val function = color.substring(0, open)
        val values = CSS_NUMBER.findAll(color.substring(open + 1)).map { it.value }.toList()
        if (values.size < 3) return null
        fun number(text: String) = text.removeSuffix("%").toDoubleOrNull() ?: Double.NaN
        fun fraction(text: String) = if (text.endsWith('%')) number(text) / 100 else number(text)
        val alpha = values.getOrNull(3)?.let { fraction(it) }?.coerceIn(0.0, 1.0) ?: 1.0
        val channels =
            when (function) {
                "rgb",
                "rgba" ->
                    values.take(3).map {
                        if (it.endsWith('%')) number(it) * 2.55 else number(it)
                    }
                "hsl",
                "hsla" -> {
                    val hue = ((number(values[0]) % 360) + 360) % 360 / 60
                    val saturation = fraction(values[1]).coerceIn(0.0, 1.0)
                    val lightness = fraction(values[2]).coerceIn(0.0, 1.0)
                    val chroma = (1 - abs(2 * lightness - 1)) * saturation
                    val second = chroma * (1 - abs(hue % 2 - 1))
                    val (r, g, b) =
                        when (hue.toInt()) {
                            0 -> Triple(chroma, second, 0.0)
                            1 -> Triple(second, chroma, 0.0)
                            2 -> Triple(0.0, chroma, second)
                            3 -> Triple(0.0, second, chroma)
                            4 -> Triple(second, 0.0, chroma)
                            else -> Triple(chroma, 0.0, second)
                        }
                    val base = lightness - chroma / 2
                    listOf(r, g, b).map { (it + base) * 255 }
                }
                else -> return null
            }
        if (channels.any { it.isNaN() }) return null
        val (r, g, b) = channels.map { it.coerceIn(0.0, 255.0).roundToInt() }
        return ((alpha * 255).roundToInt() shl 24) or (r shl 16) or (g shl 8) or b
    }

    return when (color) {
        "transparent",
        "none" -> 0
        "currentcolor" -> ink
        else -> NAMED_COLORS[color]?.let { (0xFF000000 or it.toLong()).toInt() }
    }
}

private val NAMED_COLORS: Map<String, Int> =
    ("aliceblue f0f8ff antiquewhite faebd7 aqua 00ffff aquamarine 7fffd4 azure f0ffff beige " +
            "f5f5dc bisque ffe4c4 black 000000 blanchedalmond ffebcd blue 0000ff blueviolet " +
            "8a2be2 brown a52a2a burlywood deb887 cadetblue 5f9ea0 chartreuse 7fff00 chocolate " +
            "d2691e coral ff7f50 cornflowerblue 6495ed cornsilk fff8dc crimson dc143c cyan 00ffff " +
            "darkblue 00008b darkcyan 008b8b darkgoldenrod b8860b darkgray a9a9a9 darkgreen " +
            "006400 darkgrey a9a9a9 darkkhaki bdb76b darkmagenta 8b008b darkolivegreen 556b2f " +
            "darkorange ff8c00 darkorchid 9932cc darkred 8b0000 darksalmon e9967a darkseagreen " +
            "8fbc8f darkslateblue 483d8b darkslategray 2f4f4f darkslategrey 2f4f4f darkturquoise " +
            "00ced1 darkviolet 9400d3 deeppink ff1493 deepskyblue 00bfff dimgray 696969 dimgrey " +
            "696969 dodgerblue 1e90ff firebrick b22222 floralwhite fffaf0 forestgreen 228b22 " +
            "fuchsia ff00ff gainsboro dcdcdc ghostwhite f8f8ff gold ffd700 goldenrod daa520 gray " +
            "808080 green 008000 greenyellow adff2f grey 808080 honeydew f0fff0 hotpink ff69b4 " +
            "indianred cd5c5c indigo 4b0082 ivory fffff0 khaki f0e68c lavender e6e6fa " +
            "lavenderblush fff0f5 lawngreen 7cfc00 lemonchiffon fffacd lightblue add8e6 " +
            "lightcoral f08080 lightcyan e0ffff lightgoldenrodyellow fafad2 lightgray d3d3d3 " +
            "lightgreen 90ee90 lightgrey d3d3d3 lightpink ffb6c1 lightsalmon ffa07a " +
            "lightseagreen 20b2aa lightskyblue 87cefa lightslategray 778899 lightslategrey 778899 " +
            "lightsteelblue b0c4de lightyellow ffffe0 lime 00ff00 limegreen 32cd32 linen faf0e6 " +
            "magenta ff00ff maroon 800000 mediumaquamarine 66cdaa mediumblue 0000cd mediumorchid " +
            "ba55d3 mediumpurple 9370db mediumseagreen 3cb371 mediumslateblue 7b68ee " +
            "mediumspringgreen 00fa9a mediumturquoise 48d1cc mediumvioletred c71585 " +
            "midnightblue 191970 mintcream f5fffa mistyrose ffe4e1 moccasin ffe4b5 navajowhite " +
            "ffdead navy 000080 oldlace fdf5e6 olive 808000 olivedrab 6b8e23 orange ffa500 " +
            "orangered ff4500 orchid da70d6 palegoldenrod eee8aa palegreen 98fb98 paleturquoise " +
            "afeeee palevioletred db7093 papayawhip ffefd5 peachpuff ffdab9 peru cd853f pink " +
            "ffc0cb plum dda0dd powderblue b0e0e6 purple 800080 rebeccapurple 663399 red ff0000 " +
            "rosybrown bc8f8f royalblue 4169e1 saddlebrown 8b4513 salmon fa8072 sandybrown " +
            "f4a460 seagreen 2e8b57 seashell fff5ee sienna a0522d silver c0c0c0 skyblue 87ceeb " +
            "slateblue 6a5acd slategray 708090 slategrey 708090 snow fffafa springgreen 00ff7f " +
            "steelblue 4682b4 tan d2b48c teal 008080 thistle d8bfd8 tomato ff6347 turquoise " +
            "40e0d0 violet ee82ee wheat f5deb3 white ffffff whitesmoke f5f5f5 yellow ffff00 " +
            "yellowgreen 9acd32")
        .split(' ')
        .chunked(2)
        .associate { (name, value) -> name to value.toInt(16) }
