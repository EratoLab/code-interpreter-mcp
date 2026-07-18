import pandas as pd


INPUT_FILE = "/workspace/cities.csv"
OUTPUT_FILE = "/workspace/filtered_cities.csv"
CONTINENT = "Asia"
MIN_POPULATION = 10_000_000

cities = pd.read_csv(INPUT_FILE)

required_columns = {"city", "country", "continent", "population"}
missing_columns = required_columns.difference(cities.columns)
if missing_columns:
    raise ValueError(f"Missing required columns: {sorted(missing_columns)}")

filtered = (
    cities.loc[
        (cities["continent"] == CONTINENT)
        & (cities["population"] >= MIN_POPULATION)
    ]
    .sort_values("population", ascending=False)
    .reset_index(drop=True)
)

filtered.to_csv(OUTPUT_FILE, index=False)

print(
    f"Wrote {len(filtered)} cities in {CONTINENT} with at least "
    f"{MIN_POPULATION:,} residents to {OUTPUT_FILE}"
)

# The CSV is returned by the sandbox in `files` as `filtered_cities.csv`.
{"output_file": OUTPUT_FILE, "records": len(filtered)}
