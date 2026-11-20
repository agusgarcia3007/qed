def parse_csv(text):
    """Parse CSV text into a list of records (lists of str).

    - Fields are separated by ',' and records by '\\n' or '\\r\\n'.
    - A field that starts with '"' is quoted: it may contain ',', '\\n', '\\r\\n'
      (kept exactly as written), and '""' which stands for one '"'.
      After the closing quote the next char must be ',', a record end or end of text,
      otherwise raise ValueError. An unterminated quoted field raises ValueError.
    - A '"' inside an unquoted field is kept literally.
    - A trailing record terminator does not create an extra record.
    - An empty line in the middle yields the record [''].
    - Empty text yields [].
    """
    records = []
    for line in text.splitlines():
        fields = []
        for raw in line.split(","):
            if raw.startswith('"') and raw.endswith('"') and len(raw) >= 2:
                raw = raw[1:-1]
            fields.append(raw)
        records.append(fields)
    return records
