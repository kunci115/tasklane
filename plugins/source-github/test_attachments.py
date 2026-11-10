import os
import tempfile
import unittest
from unittest import mock

import source_github as sg

PNG = b"\x89PNG\r\n\x1a\n" + b"\x00\x00\x00\rIHDR" + (10).to_bytes(4, "big") + (10).to_bytes(4, "big") + b"\x00" * 20
PDF = b"%PDF-1.7\n%fake"
BIG_PNG = b"\x89PNG\r\n\x1a\n" + b"\x00\x00\x00\rIHDR" + (9000).to_bytes(4, "big") + (10).to_bytes(4, "big") + b"\x00" * 20
GOOD = "https://github.com/user-attachments/assets/aaaa"
SIGNED = "https://private-user-images.githubusercontent.com/1/x.png?jwt=abc&amp;b=1"
PDFURL = "https://github.com/user-attachments/files/9/report.pdf"


class Kinds(unittest.TestCase):
    def test_kind_comes_from_the_bytes(self):
        self.assertEqual(sg.file_kind(PNG)[0], "png")
        self.assertEqual(sg.file_kind(PDF)[0], "pdf")
        self.assertEqual(sg.file_kind(b"\xff\xd8\xff\xe0abc")[0], "jpg")
        self.assertIsNone(sg.file_kind(b"<svg onload=alert(1)>"))
        self.assertIsNone(sg.file_kind(b"MZ\x90\x00"))

    def test_only_github_attachment_hosts(self):
        ok = sg.attachment_host_ok
        self.assertTrue(ok(GOOD))
        self.assertTrue(ok("https://private-user-images.githubusercontent.com/1/x.png"))
        for bad in ("http://github.com/user-attachments/assets/a", "https://github.com/acme/repo/raw/main/a.png",
                    "https://evil.example/a.png", "https://githubusercontent.com.evil.example/a.png",
                    "https://user@github.com/user-attachments/a", "https://github.com:8443/user-attachments/a",
                    "https://169.254.169.254/latest"):
            self.assertFalse(ok(bad), bad)


class Fetch(unittest.TestCase):
    def run_op(self, body_html, comments=(), blobs=None, trusted=True, **extra):
        blobs = blobs or {}
        issue = mock.Mock(data={"body_html": body_html, "user": {"login": "ann"}})
        dest = os.path.join(tempfile.mkdtemp(prefix="att-"), "t1")
        def fetch(url, max_bytes):
            if url not in blobs:
                raise sg.permanent("not a GitHub attachment address")
            return blobs[url]
        with mock.patch.object(sg, "app_api", return_value=issue), \
             mock.patch.object(sg, "app_paginate", return_value=list(comments)), \
             mock.patch.object(sg, "author_trusted", side_effect=lambda x: trusted if x is not None and x.get("user", {}).get("login") == "ann" else False), \
             mock.patch.object(sg, "is_own_comment", return_value=False), \
             mock.patch.object(sg, "fetch_attachment", side_effect=fetch):
            r = sg.op_fetch_attachments({"service": "acme/one", "ref": "3", "dest": dest, **extra})
        return r, dest

    def test_downloads_pictures_and_pdfs_from_body_and_comments(self):
        html = '<img src="%s"> <a href="%s">report.pdf</a> <a href="https://evil.example/x.png">x</a>' % (GOOD, PDFURL)
        comment = {"user": {"login": "ann"}, "body_html": '<img src="%s">' % SIGNED}
        r, dest = self.run_op(html, [comment], {GOOD: PNG, PDFURL: PDF, SIGNED.replace("&amp;", "&"): PNG})
        self.assertEqual([f["type"] for f in r["files"]], ["png", "pdf", "png"])
        self.assertEqual(sorted(os.listdir(dest)), ["1.png", "2.pdf", "3.png"])

    def test_a_disguised_file_and_a_huge_picture_are_refused(self):
        html = '<img src="%s"><img src="%s">' % (GOOD, PDFURL)
        r, dest = self.run_op(html, blobs={GOOD: b"<svg/>", PDFURL: BIG_PNG})
        self.assertEqual(r["files"], [])
        self.assertEqual(os.listdir(dest), [])
        self.assertEqual(len(r["skipped"]), 2)

    def test_count_limit_and_duplicates(self):
        urls = ["https://github.com/user-attachments/assets/%d" % i for i in range(6)]
        html = "".join('<img src="%s">' % u for u in urls) + '<img src="%s">' % urls[0]
        r, _ = self.run_op(html, blobs={u: PNG for u in urls}, max_files=3)
        self.assertEqual(len(r["files"]), 3)
        self.assertTrue(any("limit" in s for s in r["skipped"]))

    def test_trusted_only_leaves_a_strangers_files_out(self):
        html = '<img src="%s">' % GOOD
        r, _ = self.run_op(html, blobs={GOOD: PNG}, trusted=False, trusted_only=True)
        self.assertEqual(r["files"], [])
        r, _ = self.run_op(html, blobs={GOOD: PNG}, trusted=False)
        self.assertEqual(len(r["files"]), 1)

    def test_dest_must_be_absolute(self):
        with self.assertRaises(sg.PluginError):
            sg.op_fetch_attachments({"service": "acme/one", "ref": "3", "dest": "../x"})


if __name__ == "__main__":
    unittest.main()
