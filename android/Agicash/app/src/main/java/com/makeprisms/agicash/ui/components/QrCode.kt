package com.makeprisms.agicash.ui.components

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import android.graphics.Bitmap

/**
 * Self-contained byte-mode QR encoder. Compose has no built-in QR
 * generator (iOS uses CoreImage's `CIQRCodeGenerator`); rather than
 * pull a new Gradle dependency (ZXing) into this lane — which would
 * touch the shared version catalog and add a network-fetched artifact —
 * this is a minimal, dependency-free encoder sufficient for the BOLT-11
 * invoices the Lightning-receive flow renders.
 *
 * Scope: byte mode, error-correction level M, automatic version
 * selection up to version 20 (covers BOLT-11 invoices comfortably —
 * a typical invoice is ~250-400 chars and v20-M holds 666 bytes).
 * Not a general-purpose library; just enough to draw a scannable
 * invoice QR.
 *
 * Algorithm references: ISO/IEC 18004. The Reed-Solomon, mask, and
 * matrix-placement steps follow the standard QR construction.
 */
object QrCode {

    /**
     * Encode [content] into a square [ImageBitmap]. [sizePx] is the
     * output bitmap edge in pixels; the module grid is nearest-neighbor
     * scaled into it (crisp hard edges, like iOS's
     * `interpolation(.none)`). Returns null if the content doesn't fit
     * in the supported version range.
     */
    fun encode(
        content: String,
        sizePx: Int,
        dark: Color = Color.Black,
        light: Color = Color.White,
    ): ImageBitmap? {
        val modules = buildModules(content) ?: return null
        val n = modules.size
        val quiet = 4
        val total = n + quiet * 2
        val scale = (sizePx / total).coerceAtLeast(1)
        val dim = total * scale
        val darkArgb = dark.toArgb()
        val lightArgb = light.toArgb()
        val pixels = IntArray(dim * dim) { lightArgb }
        for (r in 0 until n) {
            for (c in 0 until n) {
                if (!modules[r][c]) continue
                val x0 = (c + quiet) * scale
                val y0 = (r + quiet) * scale
                for (dy in 0 until scale) {
                    val rowBase = (y0 + dy) * dim
                    for (dx in 0 until scale) {
                        pixels[rowBase + x0 + dx] = darkArgb
                    }
                }
            }
        }
        val bmp = Bitmap.createBitmap(dim, dim, Bitmap.Config.ARGB_8888)
        bmp.setPixels(pixels, 0, dim, 0, 0, dim, dim)
        return bmp.asImageBitmap()
    }

    private fun Color.toArgb(): Int {
        val a = (alpha * 255f + 0.5f).toInt()
        val r = (red * 255f + 0.5f).toInt()
        val g = (green * 255f + 0.5f).toInt()
        val b = (blue * 255f + 0.5f).toInt()
        return (a shl 24) or (r shl 16) or (g shl 8) or b
    }

    // ---- QR construction ----

    // EC level M *data* codeword capacities by version (ISO/IEC 18004
    // Table 9). MUST equal `g1Blocks*g1Cw + g2Blocks*g2Cw` from ECB_M
    // for the same version — buildModules pads `dataCw` to CAPACITY_M[v]
    // and then re-splits it into ECB_M blocks, so any shortfall makes
    // the block-split index past the end of `dataCw`
    // (ArrayIndexOutOfBoundsException). The previous table held the
    // pre-EC byte budget (a few codewords short of the true data
    // capacity at every version), which crashed QR rendering for any
    // payload long enough to reach the affected version — i.e. every
    // BOLT-11 invoice / Cashu token the receive & send flows draw.
    private val CAPACITY_M = intArrayOf(
        0, 16, 28, 44, 64, 86, 108, 124, 154, 182, 216,
        254, 290, 334, 365, 415, 453, 507, 563, 627, 669,
    )

    // (ecCodewordsPerBlock, numBlocksGroup1, dataCwGroup1,
    //  numBlocksGroup2, dataCwGroup2) for EC level M, versions 1..20.
    private val ECB_M = arrayOf(
        intArrayOf(0, 0, 0, 0, 0),
        intArrayOf(10, 1, 16, 0, 0),
        intArrayOf(16, 1, 28, 0, 0),
        intArrayOf(26, 1, 44, 0, 0),
        intArrayOf(18, 2, 32, 0, 0),
        intArrayOf(24, 2, 43, 0, 0),
        intArrayOf(16, 4, 27, 0, 0),
        intArrayOf(18, 4, 31, 0, 0),
        intArrayOf(22, 2, 38, 2, 39),
        intArrayOf(22, 3, 36, 2, 37),
        intArrayOf(26, 4, 43, 1, 44),
        intArrayOf(30, 1, 50, 4, 51),
        intArrayOf(22, 6, 36, 2, 37),
        intArrayOf(22, 8, 37, 1, 38),
        intArrayOf(24, 4, 40, 5, 41),
        intArrayOf(24, 5, 41, 5, 42),
        intArrayOf(28, 7, 45, 3, 46),
        intArrayOf(28, 10, 46, 1, 47),
        intArrayOf(26, 9, 43, 4, 44),
        intArrayOf(26, 3, 44, 11, 45),
        intArrayOf(26, 3, 41, 13, 42),
    )

    private fun buildModules(content: String): Array<BooleanArray>? {
        val data = content.toByteArray(Charsets.ISO_8859_1)
        var version = -1
        for (v in 1..20) {
            if (data.size + headerOverheadBytes(v) <= CAPACITY_M[v]) {
                version = v
                break
            }
        }
        if (version == -1) return null

        val totalDataCw = CAPACITY_M[version]
        val bits = BitBuffer()
        // Mode indicator: byte mode = 0100
        bits.append(0b0100, 4)
        // Character count indicator: 8 bits (v1-9) / 16 bits (v10+).
        val countBits = if (version <= 9) 8 else 16
        bits.append(data.size, countBits)
        for (b in data) bits.append(b.toInt() and 0xFF, 8)
        // Terminator (up to 4 zero bits) without overrunning capacity.
        val capacityBits = totalDataCw * 8
        val term = minOf(4, capacityBits - bits.size)
        if (term > 0) bits.append(0, term)
        // Pad to a byte boundary.
        while (bits.size % 8 != 0) bits.append(0, 1)
        // Pad bytes alternating 0xEC / 0x11.
        var pad = 0xEC
        while (bits.size / 8 < totalDataCw) {
            bits.append(pad, 8)
            pad = if (pad == 0xEC) 0x11 else 0xEC
        }

        val dataCw = bits.toBytes()
        val ecb = ECB_M[version]
        val ecPerBlock = ecb[0]
        val g1Blocks = ecb[1]
        val g1Cw = ecb[2]
        val g2Blocks = ecb[3]
        val g2Cw = ecb[4]

        val dataBlocks = ArrayList<IntArray>()
        val ecBlocks = ArrayList<IntArray>()
        var offset = 0
        for (i in 0 until g1Blocks + g2Blocks) {
            val cwCount = if (i < g1Blocks) g1Cw else g2Cw
            val block = IntArray(cwCount) { dataCw[offset + it] and 0xFF }
            offset += cwCount
            dataBlocks.add(block)
            ecBlocks.add(reedSolomon(block, ecPerBlock))
        }

        // Interleave data then EC codewords.
        val finalCw = ArrayList<Int>()
        val maxData = maxOf(g1Cw, g2Cw)
        for (i in 0 until maxData) {
            for (block in dataBlocks) {
                if (i < block.size) finalCw.add(block[i])
            }
        }
        for (i in 0 until ecPerBlock) {
            for (block in ecBlocks) finalCw.add(block[i])
        }

        val size = 17 + version * 4
        val matrix = Array(size) { arrayOfNulls<Boolean>(size) }
        placeFunctionPatterns(matrix, version, size)
        placeData(matrix, finalCw, size)

        // Pick the mask with the lowest penalty.
        var bestPenalty = Int.MAX_VALUE
        var bestMatrix: Array<BooleanArray>? = null
        for (mask in 0..7) {
            val m = applyMaskAndFormat(matrix, size, mask, version)
            val p = penalty(m, size)
            if (p < bestPenalty) {
                bestPenalty = p
                bestMatrix = m
            }
        }
        return bestMatrix
    }

    private fun headerOverheadBytes(version: Int): Int {
        // mode(4) + count(8 or 16) + terminator(4) rounded up — 3 bytes
        // for v1-9, 4 bytes for v10+. A small constant; capacity table
        // already nets data codewords.
        return if (version <= 9) 3 else 4
    }

    private class BitBuffer {
        private val bits = ArrayList<Boolean>()
        val size get() = bits.size
        fun append(value: Int, len: Int) {
            for (i in len - 1 downTo 0) {
                bits.add((value ushr i) and 1 == 1)
            }
        }
        fun toBytes(): IntArray {
            val out = IntArray(bits.size / 8)
            for (i in out.indices) {
                var b = 0
                for (j in 0 until 8) {
                    b = (b shl 1) or if (bits[i * 8 + j]) 1 else 0
                }
                out[i] = b
            }
            return out
        }
    }

    // GF(256) tables for Reed-Solomon, primitive polynomial 0x11D.
    private val GF_EXP = IntArray(512)
    private val GF_LOG = IntArray(256)

    init {
        var x = 1
        for (i in 0 until 255) {
            GF_EXP[i] = x
            GF_LOG[x] = i
            x = x shl 1
            if (x and 0x100 != 0) x = x xor 0x11D
        }
        for (i in 255 until 512) GF_EXP[i] = GF_EXP[i - 255]
    }

    private fun gfMul(a: Int, b: Int): Int {
        if (a == 0 || b == 0) return 0
        return GF_EXP[GF_LOG[a] + GF_LOG[b]]
    }

    private fun reedSolomon(data: IntArray, ecLen: Int): IntArray {
        // Generator polynomial.
        val gen = IntArray(ecLen + 1)
        gen[0] = 1
        for (i in 0 until ecLen) {
            for (j in i downTo 0) {
                gen[j + 1] = gen[j + 1] xor gfMul(gen[j], GF_EXP[i])
            }
        }
        val res = IntArray(data.size + ecLen)
        for (i in data.indices) res[i] = data[i]
        for (i in data.indices) {
            val coef = res[i]
            if (coef == 0) continue
            for (j in 0..ecLen) {
                res[i + j] = res[i + j] xor gfMul(gen[j], coef)
            }
        }
        return res.copyOfRange(data.size, data.size + ecLen)
    }

    private fun placeFunctionPatterns(
        m: Array<Array<Boolean?>>,
        version: Int,
        size: Int,
    ) {
        fun finder(r: Int, c: Int) {
            for (i in -1..7) for (j in -1..7) {
                val rr = r + i
                val cc = c + j
                if (rr !in 0 until size || cc !in 0 until size) continue
                val on = i in 0..6 && j in 0..6 &&
                    (i == 0 || i == 6 || j == 0 || j == 6 ||
                        (i in 2..4 && j in 2..4))
                m[rr][cc] = on
            }
        }
        finder(0, 0)
        finder(0, size - 7)
        finder(size - 7, 0)

        // Timing patterns.
        for (i in 8 until size - 8) {
            val v = i % 2 == 0
            if (m[6][i] == null) m[6][i] = v
            if (m[i][6] == null) m[i][6] = v
        }

        // Alignment patterns.
        val centers = alignmentCenters(version)
        for (ar in centers) for (ac in centers) {
            if ((ar <= 8 && ac <= 8) ||
                (ar <= 8 && ac >= size - 9) ||
                (ar >= size - 9 && ac <= 8)
            ) {
                continue
            }
            for (i in -2..2) for (j in -2..2) {
                val on = i == -2 || i == 2 || j == -2 || j == 2 ||
                    (i == 0 && j == 0)
                m[ar + i][ac + j] = on
            }
        }

        // Dark module.
        m[size - 8][8] = true

        // Reserve format-info area (set later) so data placement skips
        // it. Mark as a non-null sentinel via false; the format bits
        // are written in applyMaskAndFormat.
        for (i in 0..8) {
            if (m[8][i] == null) m[8][i] = false
            if (m[i][8] == null) m[i][8] = false
        }
        for (i in 0..7) {
            if (m[size - 1 - i][8] == null) m[size - 1 - i][8] = false
            if (m[8][size - 1 - i] == null) m[8][size - 1 - i] = false
        }

        // Version info (v >= 7) — reserve the two 6x3 blocks.
        if (version >= 7) {
            val vbits = versionInfoBits(version)
            for (i in 0..17) {
                val bit = (vbits ushr i) and 1 == 1
                val a = i / 3
                val b = i % 3
                m[size - 11 + b][a] = bit
                m[a][size - 11 + b] = bit
            }
        }
    }

    private fun alignmentCenters(version: Int): IntArray {
        if (version == 1) return IntArray(0)
        val table = mapOf(
            2 to intArrayOf(6, 18), 3 to intArrayOf(6, 22),
            4 to intArrayOf(6, 26), 5 to intArrayOf(6, 30),
            6 to intArrayOf(6, 34), 7 to intArrayOf(6, 22, 38),
            8 to intArrayOf(6, 24, 42), 9 to intArrayOf(6, 26, 46),
            10 to intArrayOf(6, 28, 50), 11 to intArrayOf(6, 30, 54),
            12 to intArrayOf(6, 32, 58), 13 to intArrayOf(6, 34, 62),
            14 to intArrayOf(6, 26, 46, 66), 15 to intArrayOf(6, 26, 48, 70),
            16 to intArrayOf(6, 26, 50, 74), 17 to intArrayOf(6, 30, 54, 78),
            18 to intArrayOf(6, 30, 56, 82), 19 to intArrayOf(6, 30, 58, 86),
            20 to intArrayOf(6, 34, 62, 90),
        )
        return table[version] ?: IntArray(0)
    }

    private fun versionInfoBits(version: Int): Int {
        var d = version shl 12
        val g = 0x1F25
        var bch = d
        for (i in 17 downTo 12) {
            if ((bch ushr i) and 1 == 1) {
                bch = bch xor (g shl (i - 12))
            }
        }
        return d or (bch and 0xFFF)
    }

    private fun placeData(
        m: Array<Array<Boolean?>>,
        cw: List<Int>,
        size: Int,
    ) {
        // Canonical zig-zag traversal: walk column pairs right-to-left,
        // alternating upward/downward, skipping the vertical timing
        // column and any cell already claimed by a function pattern.
        var bitIndex = 0
        val totalBits = cw.size * 8
        var c = size - 1
        var goingUp = true
        while (c > 0) {
            if (c == 6) c--
            val rows = if (goingUp) (size - 1 downTo 0) else (0 until size)
            for (r in rows) {
                for (dc in 0..1) {
                    val cc = c - dc
                    if (m[r][cc] != null) continue
                    val bit = if (bitIndex < totalBits) {
                        val byte = cw[bitIndex / 8]
                        (byte ushr (7 - (bitIndex % 8))) and 1 == 1
                    } else {
                        false
                    }
                    bitIndex++
                    m[r][cc] = bit
                }
            }
            goingUp = !goingUp
            c -= 2
        }
    }

    private fun applyMaskAndFormat(
        src: Array<Array<Boolean?>>,
        size: Int,
        mask: Int,
        version: Int,
    ): Array<BooleanArray> {
        val out = Array(size) { r -> BooleanArray(size) { c -> src[r][c] == true } }
        // Reserved (function) cells must not be masked. Recompute the
        // function map identically to placeFunctionPatterns coverage.
        val reserved = functionMap(version, size)
        for (r in 0 until size) {
            for (c in 0 until size) {
                if (reserved[r][c]) continue
                val flip = when (mask) {
                    0 -> (r + c) % 2 == 0
                    1 -> r % 2 == 0
                    2 -> c % 3 == 0
                    3 -> (r + c) % 3 == 0
                    4 -> (r / 2 + c / 3) % 2 == 0
                    5 -> (r * c) % 2 + (r * c) % 3 == 0
                    6 -> ((r * c) % 2 + (r * c) % 3) % 2 == 0
                    else -> ((r + c) % 2 + (r * c) % 3) % 2 == 0
                }
                if (flip) out[r][c] = !out[r][c]
            }
        }
        writeFormatInfo(out, size, mask)
        return out
    }

    private fun functionMap(version: Int, size: Int): Array<BooleanArray> {
        val f = Array(size) { BooleanArray(size) }
        fun rect(r0: Int, c0: Int, r1: Int, c1: Int) {
            for (r in r0..r1) for (c in c0..c1) {
                if (r in 0 until size && c in 0 until size) f[r][c] = true
            }
        }
        rect(0, 0, 8, 8)
        rect(0, size - 8, 8, size - 1)
        rect(size - 8, 0, size - 1, 8)
        for (i in 0 until size) {
            f[6][i] = true
            f[i][6] = true
        }
        val centers = alignmentCenters(version)
        for (ar in centers) for (ac in centers) {
            if ((ar <= 8 && ac <= 8) ||
                (ar <= 8 && ac >= size - 9) ||
                (ar >= size - 9 && ac <= 8)
            ) {
                continue
            }
            rect(ar - 2, ac - 2, ar + 2, ac + 2)
        }
        if (version >= 7) {
            rect(0, size - 11, 5, size - 9)
            rect(size - 11, 0, size - 9, 5)
        }
        return f
    }

    private fun writeFormatInfo(
        m: Array<BooleanArray>,
        size: Int,
        mask: Int,
    ) {
        // EC level M = 0b00. Format = (ec << 3) | mask, BCH(15,5) +
        // XOR mask 0x5412.
        val ec = 0b00
        val data = (ec shl 3) or mask
        var bch = data shl 10
        val g = 0x537
        for (i in 14 downTo 10) {
            if ((bch ushr i) and 1 == 1) bch = bch xor (g shl (i - 10))
        }
        val format = ((data shl 10) or (bch and 0x3FF)) xor 0x5412
        for (i in 0..14) {
            val bit = (format ushr i) and 1 == 1
            // Around top-left.
            when {
                i < 6 -> m[8][i] = bit
                i == 6 -> m[8][7] = bit
                i == 7 -> m[8][8] = bit
                i == 8 -> m[7][8] = bit
                else -> m[14 - i][8] = bit
            }
            // Around the other two finders (copy).
            when {
                i < 8 -> m[size - 1 - i][8] = bit
                else -> m[8][size - 15 + i] = bit
            }
        }
        m[size - 8][8] = true
    }

    private fun penalty(m: Array<BooleanArray>, size: Int): Int {
        var p = 0
        // Rule 1: runs of 5+ same color in row/col.
        for (r in 0 until size) {
            var run = 1
            for (c in 1 until size) {
                if (m[r][c] == m[r][c - 1]) {
                    run++
                    if (run == 5) p += 3 else if (run > 5) p++
                } else {
                    run = 1
                }
            }
        }
        for (c in 0 until size) {
            var run = 1
            for (r in 1 until size) {
                if (m[r][c] == m[r - 1][c]) {
                    run++
                    if (run == 5) p += 3 else if (run > 5) p++
                } else {
                    run = 1
                }
            }
        }
        // Rule 2: 2x2 blocks.
        for (r in 0 until size - 1) {
            for (c in 0 until size - 1) {
                val v = m[r][c]
                if (v == m[r][c + 1] && v == m[r + 1][c] && v == m[r + 1][c + 1]) {
                    p += 3
                }
            }
        }
        return p
    }
}
