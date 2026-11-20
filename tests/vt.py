"""Minimal VT100 screen for tests: enough of the escape set qed uses to assert on what a
person would actually see (cursor moves, erase, autowrap), with unlimited scrollback."""
import re

CSI = re.compile(r"\x1b\[([?0-9;]*)([A-Za-z@`~])")


class Screen:
    def __init__(self, cols):
        self.cols = cols
        self.rows = [[]]
        self.y = 0
        self.x = 0
        self.pending_wrap = False

    def _row(self):
        while len(self.rows) <= self.y:
            self.rows.append([])
        r = self.rows[self.y]
        while len(r) < self.x:
            r.append(" ")
        return r

    def put(self, ch):
        if self.pending_wrap:
            self.y += 1
            self.x = 0
            self.pending_wrap = False
        r = self._row()
        if self.x < len(r):
            r[self.x] = ch
        else:
            r.append(ch)
        w = 2 if ("ᄀ" <= ch <= "ᅟ" or "⺀" <= ch <= "꓏" or "가" <= ch <= "힣"
                  or "豈" <= ch <= "﫿" or "\U0001f300" <= ch <= "\U0001faff") else 1
        if w == 2:
            r.insert(self.x + 1, "")
        self.x += w
        if self.x >= self.cols:
            self.x = self.cols - 1
            self.pending_wrap = True

    def feed(self, s):
        i = 0
        while i < len(s):
            c = s[i]
            if c == "\x1b":
                m = CSI.match(s, i)
                if m:
                    self.csi(m.group(1), m.group(2))
                    i = m.end()
                    continue
                i += 2
                continue
            if c == "\r":
                self.x = 0
                self.pending_wrap = False
            elif c == "\n":
                self.y += 1
                self.pending_wrap = False
            elif c == "\b":
                self.x = max(0, self.x - 1)
            elif c >= " ":
                self.put(c)
            i += 1

    def csi(self, args, op):
        if args.startswith("?"):
            return
        n = int(args.split(";")[0]) if args and args.split(";")[0].isdigit() else None
        self.pending_wrap = False
        if op == "A":
            self.y = max(0, self.y - (n or 1))
        elif op == "B":
            self.y += n or 1
        elif op == "C":
            self.x = min(self.cols - 1, self.x + (n or 1))
        elif op == "D":
            self.x = max(0, self.x - (n or 1))
        elif op == "G":
            self.x = (n or 1) - 1
        elif op == "K":
            r = self._row()
            if n == 2:
                r.clear()
            elif not n:
                del r[self.x:]
        elif op == "J":
            if n == 2 or n == 3:
                self.rows = [[]]
                self.y = 0
            else:
                del self._row()[self.x:]
                del self.rows[self.y + 1:]
        elif op == "H":
            self.y, self.x = 0, 0

    def lines(self):
        return ["".join(r).rstrip() for r in self.rows]

    def text(self):
        return "\n".join(self.lines())
