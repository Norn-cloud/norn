"""Non-executing TypeScript/JavaScript syntax substrate."""
import json
from tree_sitter import Language, Parser
import tree_sitter_typescript


def parse(data: bytes, tsx=False):
    language = tree_sitter_typescript.language_tsx() if tsx else tree_sitter_typescript.language_typescript()
    return Parser(Language(language)).parse(data).root_node


def walk(node):
    yield node
    for child in node.named_children:
        yield from walk(child)


def spelling(node):
    return node.text.decode() if node else ""


def string(node):
    if node is None or node.type != "string":
        return None
    text = spelling(node)
    # Decode escapes without evaluating source.
    if text.startswith('"'):
        return json.loads(text)
    if "\\" not in text:
        return text[1:-1]
    return None
