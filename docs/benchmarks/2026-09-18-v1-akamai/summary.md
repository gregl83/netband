# NDT7 comparison summary

Only runs with exit code 0, outcome `success`, and both throughput values are included
in the distribution statistics. CV is the sample standard deviation divided by the mean.

| Client | Complete runs | Auxiliary diagnostics | Download median (p10–p90) | Download CV | Upload median (p10–p90) | Upload CV |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| reference | 20/20 (100.0%) | 0 | 208.42 (185.99–243.83) Mbit/s | 12.09% | 110.20 (101.51–116.77) Mbit/s | 9.14% |
| netband | 20/20 (100.0%) | 0 | 209.76 (193.98–277.51) Mbit/s | 16.18% | 114.50 (102.34–117.52) Mbit/s | 5.40% |

| Direction | Paired runs | Median signed difference | Median absolute difference | p90 absolute difference |
| --- | ---: | ---: | ---: | ---: |
| download | 20 | -0.36% | 7.40% | 37.48% |
| upload | 20 | 2.81% | 3.91% | 12.78% |

Signed difference is `(Netband - reference) / reference × 100` for measurements
with the same pair number.
