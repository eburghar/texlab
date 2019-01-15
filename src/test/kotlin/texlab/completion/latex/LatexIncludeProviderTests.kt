package texlab.completion.latex

import org.junit.jupiter.api.Assertions.assertArrayEquals
import org.junit.jupiter.api.Test
import texlab.WorkspaceBuilder

class LatexIncludeProviderTests {
    @Test
    fun `it should exclude files that are already included`() {
        val builder = WorkspaceBuilder()
                .document("foo.tex", "\\include{bar}\n\\include{}")
                .document("bar.tex", "")
                .document("baz.tex", "")

        val provider = LatexIncludeProvider(builder.workspace)

        val expected = arrayOf("baz")
        val actual = provider
                .complete(builder.completion("foo.tex", 1, 9))
                .map { it.label }
                .toTypedArray()
        assertArrayEquals(expected, actual)
    }
}

