# NDT7 comparison summary

Only runs with exit code 0, outcome `success`, and both throughput values are included
in the distribution statistics. CV is the sample standard deviation divided by the mean.

| Client | Complete runs | Auxiliary diagnostics | Download median (p10–p90) | Download CV | Upload median (p10–p90) | Upload CV |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| reference | 20/20 (100.0%) | 0 | 240.77 (196.18–304.57) Mbit/s | 18.88% | 109.31 (101.98–118.05) Mbit/s | 6.31% |
| netband | 20/20 (100.0%) | 0 | 247.05 (208.30–328.28) Mbit/s | 17.57% | 114.28 (101.45–116.70) Mbit/s | 8.02% |

| Direction | Paired runs | Median signed difference | Median absolute difference | p90 absolute difference |
| --- | ---: | ---: | ---: | ---: |
| download | 20 | 8.70% | 12.60% | 41.74% |
| upload | 20 | -0.26% | 4.57% | 15.37% |

Signed difference is `(Netband - reference) / reference × 100` for measurements
with the same pair number.
