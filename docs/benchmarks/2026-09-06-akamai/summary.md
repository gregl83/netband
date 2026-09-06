# NDT7 comparison summary

Only runs with exit code 0, outcome `success`, and both throughput values are included
in the distribution statistics. CV is the sample standard deviation divided by the mean.

| Client | Complete runs | Auxiliary diagnostics | Download median (p10–p90) | Download CV | Upload median (p10–p90) | Upload CV |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| reference | 20/20 (100.0%) | 0 | 49.72 (47.58–54.49) Mbit/s | 6.52% | 19.44 (17.08–20.77) Mbit/s | 10.86% |
| netband | 20/20 (100.0%) | 0 | 53.17 (48.79–56.19) Mbit/s | 6.23% | 20.56 (19.30–22.65) Mbit/s | 6.63% |

| Direction | Paired runs | Median signed difference | Median absolute difference | p90 absolute difference |
| --- | ---: | ---: | ---: | ---: |
| download | 20 | 7.28% | 9.15% | 17.51% |
| upload | 20 | 6.68% | 7.32% | 25.17% |

Signed difference is `(Netband - reference) / reference × 100` for measurements
with the same pair number.
