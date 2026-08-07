from pathlib import Path

path = Path('app/ui/TemplateFields.tsx')
text = path.read_text(encoding='utf-8')
old = """    return {\n      ...type,\n      children: type.children.map(cType => {\n"""
new = """    return {\n      ...type,\n      label: type.name,\n      value: type.id,\n      children: type.children.map(cType => {\n"""
if old not in text:
    raise SystemExit('TemplateFields partition tree pattern not found')
path.write_text(text.replace(old, new, 1), encoding='utf-8')
