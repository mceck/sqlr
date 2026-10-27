"""Check public manual rendering and navigation without external dependencies."""
import importlib.util
from pathlib import Path
import unittest
import tempfile
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('build_docs', Path(__file__).parents[1] / 'build-docs.py')
docs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(docs)


class ManualTests(unittest.TestCase):
    def test_manual_navigation_includes_every_page(self):
        self.assertEqual(docs.PAGES, {p.relative_to(docs.DOCS).as_posix() for p in (docs.DOCS / 'manual').rglob('*.md')})
        self.assertIn('manual/agents.md', docs.PAGES)
        self.assertIn('manual/glossary.md', docs.PAGES)

    def test_rewrites_local_links_and_preserves_anchors(self):
        here = docs.DOCS / 'manual/getting-started.md'
        self.assertEqual(docs.rewrite('connections.md#tls', here), 'connections.html#tls')
        self.assertEqual(docs.rewrite('https://example.com/a.md', here), 'https://example.com/a.md')

    def test_list_ends_before_following_block(self):
        body, _ = docs.render('1. Open it.\n2. Run it.\n\n## Results\n\nDone.', docs.DOCS / 'manual/README.md')
        self.assertEqual(body.count('<ol>'), 1)
        self.assertIn('</ol>\n<h2', body)
        body, _ = docs.render('- First\n\n- Second\n\n---\n\nNext', docs.DOCS / 'manual/README.md')
        self.assertEqual(body.count('<ul>'), 1)
        self.assertIn('<hr>', body)

    def test_diagrams_and_comments(self):
        body, _ = docs.render('```mermaid\nflowchart LR\n  A --> B\n```\n\n<!-- screenshot: missing -->', docs.DOCS / 'manual/README.md')
        self.assertIn('class="mermaid"', body)
        self.assertNotIn('<p>&lt;!--', body)

    def test_validation_rejects_broken_headings_and_unused_images(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manual = root / 'manual'
            (manual / 'images').mkdir(parents=True)
            (manual / 'README.md').write_text('# Manual\n\n[Topic](topic.md#missing)')
            (manual / 'topic.md').write_text('# Topic\n\n[Manual](README.md)')
            (manual / 'images/unused.png').touch()
            with patch.object(docs, 'DOCS', root), patch.object(docs, 'PAGES', {'manual/README.md', 'manual/topic.md'}):
                with self.assertRaises(SystemExit) as error:
                    docs.validate_manual()
                self.assertIn('missing heading', str(error.exception))
                self.assertIn('Unused image', str(error.exception))

    def test_attribute_values_are_escaped(self):
        body = docs.inline('[link](https://example.com/"x) ![alt "quote"](images/a.png)', docs.DOCS / 'manual/README.md')
        self.assertIn('&quot;x', body)
        self.assertIn('alt="alt &quot;quote&quot;"', body)


if __name__ == '__main__':
    unittest.main()
