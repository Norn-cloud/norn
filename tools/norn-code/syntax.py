"""Non-executing TypeScript/JavaScript syntax substrate."""
import re
from tree_sitter import Language, Parser
import tree_sitter_typescript


def parse(data: bytes, tsx=False):
    language = tree_sitter_typescript.language_tsx() if tsx else tree_sitter_typescript.language_typescript()
    return Parser(Language(language)).parse(data).root_node


def walk(node):
    stack = [node]
    while stack:
        current = stack.pop()
        yield current
        stack.extend(reversed(current.named_children))


def spelling(node):
    return node.text.decode() if node else ""


def string(node):
    if node is None or node.type != "string":
        return None
    text = spelling(node)[1:-1]
    escapes = {'n': '\n', 'r': '\r', 't': '\t', 'b': '\b', 'f': '\f', 'v': '\v', '0': '\0'}
    def decode(match):
        value = match.group(1)
        if value.startswith('x'):
            return chr(int(value[1:], 16))
        if value.startswith('u'):
            return chr(int(value[2:-1] if value.startswith('u{') else value[1:], 16))
        if value in {'\n', '\r', '\r\n'}:
            return ''
        return escapes.get(value, value)
    return re.sub(r'\\(x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|u[0-9a-fA-F]{4}|\r\n|[\s\S])', decode, text)
