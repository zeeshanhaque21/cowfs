# g5 differential: FAIL (FAIL)

Requested 155 cases, mode diagnostic.
Native: {'PASS': 127, 'NOT_RUN': 26, 'FAIL': 2}.
Cowfs: {'PASS': 76, 'NOT_RUN': 69, 'FAIL': 9, 'PASS_EMULATED': 1}.
Pairs: {'both_pass': 76, 'gap': 42, 'worse': 8, 'both_not_run': 26, 'both_fail': 1, 'emulated': 1, 'other': 1}.

Worse than native (8): generic/025 generic/087 generic/088 generic/127 generic/285 generic/426 generic/448 generic/467
Gap, native passes and cowfs does not run, inherent_fuse (4): generic/114 generic/240 generic/418 generic/538
Gap, native passes and cowfs does not run, missing_feature (38): generic/008 generic/009 generic/012 generic/016 generic/021 generic/022 generic/058 generic/060 generic/061 generic/063 generic/072 generic/078 generic/092 generic/094 generic/213 generic/228 generic/255 generic/286 generic/315 generic/316 generic/349 generic/350 generic/351 generic/389 generic/404 generic/420 generic/424 generic/436 generic/445 generic/469 generic/528 generic/539 generic/545 generic/553 generic/555 generic/568 generic/586 generic/742
Emulated mount cycle on cowfs: generic/247
Cowfs not-run by class: {'missing_feature': 44, 'by_fstype': 7, 'inherent_fuse': 4, 'harness': 14}
Native not-run by class: {'by_fstype': 7, 'harness': 14, 'missing_feature': 5}
Control: {'status': 'FAIL', 'problems': []}
