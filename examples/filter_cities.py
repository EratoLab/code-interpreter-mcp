import pandas as pd


INPUT_FILE = "/workspace/cities.csv"
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

print(
    f"Cities in {CONTINENT} with at least "
    f"{MIN_POPULATION:,} residents: {len(filtered)}"
)
print(filtered.to_csv(index=False), end="")

# The sandbox exposes the value of the final expression as `result`.
filtered.to_dict(orient="records")
