"""Simulation of the logical-operator sequence counting in
sonar-java CognitiveComplexityVisitor and SonarJS S3776 rule.ts.

Purpose: systematically re-derive the expression -> increment table and
sanity-check it against the values hard-coded in the two projects' test files.
This is a model of the source, not the source itself.
"""


class Node:
    def __init__(self, kind, left=None, right=None, op=None, args=None, name=None):
        self.kind = kind  # 'logical','call','paren','unary','atom'
        self.left = left
        self.right = right
        self.op = op
        self.args = args or []
        self.name = name


# ---- tiny parser: && binds tighter than ||, left assoc, unary !, calls, parens ----
class Parser:
    def __init__(self, s):
        self.s = s
        self.i = 0

    def ws(self):
        while self.i < len(self.s) and self.s[self.i] == ' ':
            self.i += 1

    def peek(self):
        self.ws()
        return self.s[self.i] if self.i < len(self.s) else ''

    def eat(self, c):
        self.ws()
        assert self.s.startswith(c, self.i), (c, self.i, self.s[self.i:self.i + 5])
        self.i += len(c)

    def parse(self):
        n = self.or_expr()
        self.ws()
        assert self.i == len(self.s), ('trailing', self.s[self.i:])
        return n

    def or_expr(self):
        n = self.and_expr()
        while True:
            self.ws()
            if self.s.startswith('||', self.i):
                self.i += 2
                n = Node('logical', n, self.and_expr(), op='||')
            else:
                return n

    def and_expr(self):
        n = self.unary()
        while True:
            self.ws()
            if self.s.startswith('&&', self.i):
                self.i += 2
                n = Node('logical', n, self.unary(), op='&&')
            else:
                return n

    def unary(self):
        self.ws()
        if self.s.startswith('!', self.i):
            self.i += 1
            return Node('unary', left=self.unary())
        return self.primary()

    def primary(self):
        self.ws()
        if self.s[self.i] == '(':
            self.i += 1
            inner = self.or_expr()
            self.eat(')')
            return Node('paren', left=inner)
        # identifier, maybe call
        j = self.i
        while self.i < len(self.s) and (self.s[self.i].isalnum() or self.s[self.i] == '_'):
            self.i += 1
        name = self.s[j:self.i]
        self.ws()
        if self.i < len(self.s) and self.s[self.i] == '(':
            self.i += 1
            args = []
            if self.peek() != ')':
                args.append(self.or_expr())
                self.ws()
                while self.s[self.i] == ',':
                    self.i += 1
                    args.append(self.or_expr())
                    self.ws()
            self.eat(')')
            return Node('call', args=args, name=name)
        return Node('atom', name=name)


def strip_parens(n):
    while n is not None and n.kind == 'paren':
        n = n.left
    return n


# ---------------- sonar-java ----------------
def java_count(root):
    ignored = set()
    total = 0

    def flatten(n):
        n = strip_parens(n)
        if n is not None and n.kind == 'logical':
            ignored.add(id(n))
            return flatten(n.left) + [n] + flatten(n.right)
        return []

    def visit(n):
        nonlocal total
        if n is None:
            return
        if n.kind == 'logical' and id(n) not in ignored:
            flat = flatten(n)
            prev = None
            for cur in flat:
                if prev is None or prev.op != cur.op:
                    total += 1
                prev = cur
        if n.kind == 'logical':
            visit(n.left)
            visit(n.right)
        elif n.kind == 'paren':
            visit(n.left)
        elif n.kind == 'unary':
            visit(n.left)
        elif n.kind == 'call':
            for a in n.args:
                visit(a)

    visit(root)
    return total


# ---------------- SonarJS ----------------
def js_count(root):
    considered = set()
    total = 0

    def flatten(n):
        # ESTree has no parenthesised-expression node: parens are transparent.
        n = strip_parens(n)
        if n is not None and n.kind == 'logical':
            considered.add(id(n))
            return flatten(n.left) + [n] + flatten(n.right)
        return []

    def visit(n):
        nonlocal total
        if n is None:
            return
        if n.kind == 'logical' and id(n) not in considered:
            flat = flatten(n)
            prev = None
            for cur in flat:
                if cur.op not in ('||', '??') and (prev is None or prev.op != cur.op):
                    total += 1
                prev = cur
        if n.kind == 'logical':
            visit(n.left)
            visit(n.right)
        elif n.kind == 'paren':
            visit(n.left)
        elif n.kind == 'unary':
            visit(n.left)
        elif n.kind == 'call':
            for a in n.args:
                visit(a)

    visit(root)
    return total


def white_paper_count(root):
    """By the white paper's stated rule ("a fundamental increment for each sequence
    of binary logical operators" / "each new sequence of like operators"), with the
    boundaries established by its own examples: a unary `!` starts a fresh boolean
    sub-expression, and call arguments are separate expressions. Parentheses are
    treated as transparent (verified for sonar-java; inferred for the paper)."""
    return java_count(root)


CASES = [
    'a && b && c',
    'a || b || c || d',
    'a || b && c || d',
    'a && b || c && d',
    'a && !(b && c)',
    '(a || b) && (c || d)',
    'f(a && b) || g(c || d)',
]

# Values from the projects' own test files (primary evidence).
KNOWN = {
    'a && b && c': (1, 1),
    'a || b || c || d': (1, 0),
    'a || b && c || d': (3, 1),
    'a && b || c && d': (3, 2),
    'a && !(b && c)': (2, 2),
    'f(a && b) || g(c || d)': (3, 1),
}

print(f"{'expression':28} {'java':>5} {'js':>4} {'wp':>4}   known(java,js)")
for e in CASES:
    t = Parser(e).parse()
    j, s, w = java_count(t), js_count(t), white_paper_count(t)
    k = KNOWN.get(e)
    mark = ''
    if k is not None:
        mark = 'OK' if (j, s) == k else f'MISMATCH expected {k}'
    print(f"{e:28} {j:5} {s:4} {w:4}   {mark}")

# Cross-check against the sonar-java test file comments
JAVA_TESTS = {
    'a && b || foo(b && c)': 3,
    'a && (b || c) || d': 2,
    'a && b || c || d': 2,
    'a && b || c && d || e': 4,
    'a || b && c || d && e': 4,
    'a && b && c || d || e': 2,
    'a && b && c && d && e': 1,
    'a || b || c || d || e': 1,
    'a && b && c || d || e && f': 3,
    'a || (b || c)': 1,
    'a && b && c || d': 2,
}
print('\n-- sonar-java test file cross-check (logical-only portion) --')
for e, expected in JAVA_TESTS.items():
    got = java_count(Parser(e).parse())
    print(f"{e:32} java={got:2} expected={expected:2} {'OK' if got == expected else 'MISMATCH'}")

JAVA_MIXED = {
    'a && !(b && c) ||': None,
}
print('\n-- extraConditions12 (if condition, logical part only) --')
e = ('a && b && c || d || e && f || (h || (i && j || k)) || l || m')
print(e)
print('java logical =', java_count(Parser(e).parse()), '(test file implies 6 logical + 1 if = 7)')

# JS test-file cross-checks
JS_TESTS = {
    '1 && 2 && 3 && 4': 1,
    '(1 && 2) && (3 && 4)': 1,
    '((1 && 2) && 3) && 4': 1,
    '1 && (2 && (3 && 4))': 1,
    '1 || 2 || 3 || 4': 0,
    '1 && 2 || 3 || 4': 1,
    '1 && 2 || 3 && 4': 2,
    '1 && 2 && !(3 && 4)': 2,
    '1 && 2 || 3 && 4': 2,
}
print('\n-- SonarJS unit.test.ts cross-check --')
for e, expected in JS_TESTS.items():
    got = js_count(Parser(e).parse())
    print(f"{e:24} js={got:2} expected={expected:2} {'OK' if got == expected else 'MISMATCH'}")
