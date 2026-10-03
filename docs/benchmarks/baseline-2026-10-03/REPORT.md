# Mesure initiale Weave / Rayon / séquentiel

Référence : f0f81d30b561cfdfe70511788362770904af2ff1. Le checkout contient des corrections :
ses sources exactes sont conservées dans [source.tar.gz](source.tar.gz), avec leurs empreintes
dans [metadata.json](metadata.json). Dépendances verrouillées, compilateur et matériel enregistrés.

Pools de [1, 2, 4, 8] workers ; 2 échauffements et 9 répétitions par scénario.
Temps, latence, allocations et contention sont exécutés séparément.
Les assertions vérifient le checksum de chaque moteur sur les mêmes entrées.
L'équivalent séquentiel n'utilise pas de pool.

## Comparaisons de temps en régime établi

Médianes en ms ; dispersion et débits détaillés dans [summary.json](summary.json).

### 1 worker(s)

- submit_small : Weave 1.166 ms ; Rayon 0.076 ms ; séquentiel 0.004 ms.
- external_producers : Weave 1.059 ms ; Rayon 0.322 ms ; séquentiel 0.016 ms.
- join : Weave 0.108 ms ; Rayon 0.066 ms ; séquentiel 0.006 ms.
- map_light : Weave 1.672 ms ; Rayon 1.362 ms ; séquentiel 0.665 ms.
- map_heavy : Weave 4.015 ms ; Rayon 3.891 ms ; séquentiel 4.309 ms.
- filter_reduce : Weave 0.326 ms ; Rayon 0.176 ms ; séquentiel 0.104 ms.
- collect : Weave 0.396 ms ; Rayon 0.107 ms ; séquentiel 0.048 ms.
- memory_scattered : Weave 19.727 ms ; Rayon 17.716 ms ; séquentiel 26.061 ms.

### 2 worker(s)

- submit_small : Weave 1.699 ms ; Rayon 0.098 ms ; séquentiel 0.004 ms.
- external_producers : Weave 3.366 ms ; Rayon 0.817 ms ; séquentiel 0.016 ms.
- join : Weave 0.242 ms ; Rayon 0.084 ms ; séquentiel 0.006 ms.
- map_light : Weave 1.058 ms ; Rayon 0.554 ms ; séquentiel 0.643 ms.
- map_heavy : Weave 2.103 ms ; Rayon 2.059 ms ; séquentiel 4.043 ms.
- filter_reduce : Weave 0.293 ms ; Rayon 0.141 ms ; séquentiel 0.102 ms.
- collect : Weave 0.428 ms ; Rayon 0.124 ms ; séquentiel 0.048 ms.
- memory_scattered : Weave 9.884 ms ; Rayon 8.779 ms ; séquentiel 21.726 ms.

### 4 worker(s)

- submit_small : Weave 5.747 ms ; Rayon 0.223 ms ; séquentiel 0.004 ms.
- external_producers : Weave 4.500 ms ; Rayon 0.360 ms ; séquentiel 0.016 ms.
- join : Weave 0.401 ms ; Rayon 0.599 ms ; séquentiel 0.006 ms.
- map_light : Weave 1.312 ms ; Rayon 0.369 ms ; séquentiel 0.643 ms.
- map_heavy : Weave 1.328 ms ; Rayon 1.310 ms ; séquentiel 3.993 ms.
- filter_reduce : Weave 0.826 ms ; Rayon 0.366 ms ; séquentiel 0.102 ms.
- collect : Weave 0.951 ms ; Rayon 0.269 ms ; séquentiel 0.048 ms.
- memory_scattered : Weave 6.270 ms ; Rayon 4.610 ms ; séquentiel 21.936 ms.

### 8 worker(s)

- submit_small : Weave 18.748 ms ; Rayon 0.857 ms ; séquentiel 0.004 ms.
- external_producers : Weave 11.198 ms ; Rayon 0.563 ms ; séquentiel 0.016 ms.
- join : Weave 1.551 ms ; Rayon 0.954 ms ; séquentiel 0.006 ms.
- map_light : Weave 2.849 ms ; Rayon 0.427 ms ; séquentiel 0.682 ms.
- map_heavy : Weave 1.637 ms ; Rayon 1.084 ms ; séquentiel 4.378 ms.
- filter_reduce : Weave 1.597 ms ; Rayon 0.817 ms ; séquentiel 0.102 ms.
- collect : Weave 1.495 ms ; Rayon 2.406 ms ; séquentiel 0.048 ms.
- memory_scattered : Weave 7.359 ms ; Rayon 4.745 ms ; séquentiel 28.035 ms.

## Données et reproduction

Les fichiers time-N.csv, latency-N.csv, allocations-N.csv et contention-N.csv conservent les mesures brutes.
La création/destruction du pool est une ligne séparée de time-N.csv.
Les latences sont appel de soumission→début et appel de soumission→fin du calcul pour une rafale de 256 jobs,
hors restitution du handle et notification finale. Le verrou de collecte est hors intervalle mesuré.
Les allocations couvrent le passage et ses valeurs temporaires, tous threads confondus.
La contention mesure tentatives, try_lock occupés et temps cumulé d'attente du mutex central de Weave.
Rayon ne fournit pas un compteur comparable ; aucun chiffre de contention Rayon n'est inventé.

~~~text
CARGO_TARGET_DIR=/tmp/weave-bench-phase0 python3 benchmarks/run.py --workers 1,2,4,8 --samples 9 --output docs/benchmarks/nouvelle-mesure
~~~

Pour reproduire cette source précise, extraire source.tar.gz dans un dossier vide, installer le compilateur
enregistré et utiliser les Cargo.lock conservés. Une autre machine peut donner d'autres temps.

## Limites

- Shared WSL host; CPU frequency and competing Windows load are not controlled.
- Warmups and engine order are fixed; no statistical significance or regression thresholds claimed.
- Allocation calls/requested bytes are not peak live bytes, RSS or retained memory.
- Contention clocks/try_lock/atomics perturb scheduling; diagnostic timings are not throughput baselines.
- Rayon has no priority/submit-handle API equivalent: those rows compare checksum-producing work, not identical API semantics.
- spawn_empty includes observable accounting and a Weave completion channel; it is not pure empty dispatch.
- Cache events, bandwidth, P/E cores and multi-NUMA access require controlled hardware/profiling.
- Batch, cancellation and telemetry are not implemented; their section-9 rows are deferred.
- No remote GitHub Actions result is claimed.
