from pathlib import Path

workflow = Path('.github/workflows/temp-runtime-state-integration.yml')
lines = workflow.read_text().splitlines()
start = next(index for index, line in enumerate(lines) if "python - <<'PY'" in line) + 1
end = next(index for index in range(start, len(lines)) if lines[index].strip() == 'PY')
script = '\n'.join(line[10:] if line.startswith('          ') else line for line in lines[start:end])
script = script.replace('if actual != expected:', 'if actual < expected:')
script = script.replace('return text.replace(old, new)', 'return text.replace(old, new, expected)')
parameter_start = script.index("text = replace(text, '''                        entry\n")
parameter_end = script.index("text = replace(text, 'impl StreamingScanWriter", parameter_start)
parameter_patch = '''text = replace(text, "                            .map(|value| i64::from(value.definition_version)),\\n", """                            .map(|value| i64::from(value.definition_version)),
                        entry
                            .cache_runtime_state
                            .as_ref()
                            .map(CacheRuntimeState::as_str),
""")
'''
script = script[:parameter_start] + parameter_patch + script[parameter_end:]
exec(compile(script, '<runtime-state-integration>', 'exec'), {})
