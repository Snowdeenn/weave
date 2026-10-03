# Audit unsafe, durées de vie et phase 0

Référence de départ : f0f81d30b561cfdfe70511788362770904af2ff1.
Au moment de cet audit, seule la roadmap de origin/main à 1382f4a était importée. Les autres changements distants, notamment Send/Sync de NumaBuffer, sont intégrés ensuite et couverts par la [validation de fusion](../merge-2026-10-03/README.md).
Les sources exactes de la mesure finale sont archivées dans
[le dossier de référence](../../benchmarks/baseline-2026-10-03/REPORT.md).

## Durées de vie et TLS

[CURRENT_WORKER](../../../src/pool.rs) est un RefCell<Option<WorkerContext>> local
au thread. WorkerContext possède un Arc et un index ; aucune référence vers la
pile du worker, aucun pointeur TLS brut et aucun emprunt artificiellement static.
current_worker clone le contexte puis libère l'emprunt RefCell avant tout
callback. La boucle efface le TLS avant de terminer. Les tests de pools croisés
vérifient que l'aide coopérative conserve l'identité du worker appelant.

Les deux transmutes dispersées sont remplacées par un unique helper unsafe,
[Job::from_borrowed](../../../src/job.rs). Il efface uniquement la durée de vie du
trait object Box pour le ranger dans une file commune avec les jobs possédés.
Il ne fabrique aucune référence. Son contrat interdit toute sortie avant
consommation/destruction des captures empruntées.

Ses seuls appelants :
- join_on : une publication rejetée détruit le job ; une publication acceptée
  ne peut plus paniquer. Les deux branches sont interceptées et le handle attend
  le signal final Acquire avant retour ou reprise du panic.
- Scope::spawn_task : invariance de Scope et borne HRTB du corps. Un GroupTicket
  est enregistré avant publication. Job possède sa closure puis un guard
  Completion distinct. Le rejet détruit les captures avant le ticket ; run
  appelle la closure puis détruit le guard dans sa propre frame, aussi sur unwind.
  Le scope attend tous les tickets, y compris ceux des descendants.

Le premier passage Miri avait réussi, mais une autre graine a découvert un UB :
le ticket réglé dans la closure pouvait laisser sortir le scope avant la fin
exacte de l'appel FnOnce, dont les emprunts restaient protégés. Le
[journal avant correction](miri-before-fix.log) conserve cette découverte.
L'achèvement est maintenant signalé hors de cet appel. Les handles utilisent
la même barrière Release/Acquire : publier le résultat ne suffit pas à terminer
le wrapper. Les deux graines passent après correction, sans désactiver les
contrôles de provenance, d'emprunts ou de fuite.

Les annotations static des closures détachées sont des contraintes publiques
réelles. Celles des labels, des raisons d'erreur et des payloads Any correspondent
respectivement à des littéraux ou au contrat de panic de Rust.

## Défaut corrigé : soumission et scope

Le code initial incrémentait Group.pending puis ne le décrémentait que dans
la closure exécutée. Un unwind avant acceptation laissait un ticket fantôme :
le scope ne pouvait plus terminer. Le nettoyage est maintenant RAII.

enqueue vérifie le débordement de pending et réserve la place en file avant
publication. En cas d'erreur, le verrou est relâché avant le panic et la
destruction des captures. Après insertion et comptabilité, il n'existe plus
d'opération faillible avant retour. Le scheduler n'a pas été remplacé.

Quatre tests de [scope::failure_tests](../../../src/scope.rs) couvrent rejet de
submit avec emprunt déjà accepté, rejet capturé suivi d'une nouvelle tâche,
rejet de join et destructeur de capture qui panique. L'injection est un TLS
cfg(test), consommé avant publication ; elle n'existe pas dans la bibliothèque
de production. Elle teste le protocole de rejet sans épuiser réellement la RAM.

## Autres blocs unsafe

- [affinity/linux.rs](../../../src/affinity/linux.rs) : zeroed uniquement sur un
  bitset d'entiers ; indices bornés par CPU_SETSIZE ; taille ABI exacte et masque
  vivant pendant sched_setaffinity. Les résultats OS sont contrôlés.
- [memory/linux.rs](../../../src/memory/linux.rs) : propriétaire unique du mapping
  mmap, MAP_FAILED contrôlé, calculs de tailles vérifiés, borne isize::MAX,
  capacité en éléments et marge d'alignement. Le pointeur data reste dans le
  mapping possédé ; munmap utilise l'adresse/longueur originales. Mbind reçoit
  un masque initialisé et un instantané de nœuds ; le système valide les
  permissions finales. Ni migration ni résidence physique ne sont revendiquées.
- [memory/buffer.rs](../../../src/memory/buffer.rs) : seuls les len premiers
  éléments sont initialisés ; len ≤ capacité. Les slices sont liées aux
  emprunts partagé/exclusif du propriétaire ; push écrit un slot vierge puis
  incrémente len. Drop détruit le préfixe avant la libération du mapping.
  Le pointeur brut exporté ne prolonge aucune durée de vie.
- Les blocs des tests d'affinité/mapping ont les mêmes contraintes de masque,
  de taille et de mapping vivant. Le test mincore post-drop ne déréférence pas
  la mémoire libérée et s'isole dans un processus pour éviter sa réutilisation.
- [Allocateur des benchmarks](../../../benchmarks/src/main.rs) : les opérations
  alloc/dealloc/realloc transmettent exactement pointeurs et Layout à System.
  Seuls des compteurs atomiques sans allocation sont ajoutés. Aucun chemin
  d'allocateur ne journalise ni n'alloue. Cette instrumentation n'est compilée
  que pour le passage d'allocations, séparé du chronométrage.

## Destruction, équité et ordre

Les tests [safety_stress.rs](../../../tests/safety_stress.rs) comptent les captures
et résultats sur succès, panic, abandon du handle avant achèvement, destruction
depuis un worker et drainage des descendants. Les tests existants couvrent
également handles oubliés et destructeurs de résultats qui paniquent.

Limites explicites : mem::forget est une demande de fuite de l'utilisateur.
Si la destruction d'un payload de panic panique elle-même, discard intercepte
ce second panic et oublie son payload pour éviter un nouvel unwind incontrôlé.
Les captures ordinaires sont libérées ; la libération absolue de ressources
après abort ou avec destructeurs récursivement paniqueurs n'est pas promise.

Priorité stricte High > Normal > Low, sans préemption et **sans borne de
starvation**. Dans une priorité : LIFO local, FIFO global et vol FIFO des
victimes ; aucune équité générale entre producteurs. Un test renouvelle 64
jobs High et observe Normal/Low seulement après la fin du flux. Cette
caractérisation ne réalise pas l'anti-starvation prévue en phase 1.

collect, fill et extend préservent l'ordre de la source ; map/filter/filter_map
préservent l'ordre des éléments retenus. Les effets des callbacks n'ont pas
cet ordre. find est l'alias de find_first (première correspondance dans l'ordre
source) ; find_any peut choisir toute correspondance. L'arrêt est coopératif,
et des callbacks déjà démarrés peuvent encore finir ou paniquer.

Les réductions exigent associativité, identité neutre pour fold et cohérence
entre l'opération de feuille et la combinaison. La commutativité n'est pas
requise pour une combinaison respectant l'ordre. Pour des flottants,
l'associativité mathématique ne garantit pas l'égalité bit à bit : le test
floating_reduction_can_differ_from_sequential_grouping produit 511 en
séquentiel et 0 en parallèle sur la même entrée. Débordements d'entiers,
NaN et effets de bord demandent un contrat adapté à l'opération utilisateur.

## Validation et exclusions

Journaux conservés :
[Windows](windows.log), [Linux](linux.log), [Miri](miri.log).
Commandes et limites de mesure :
[rapport comparatif](../../benchmarks/baseline-2026-10-03/REPORT.md).

Le stress utilise la graine 20261003, 200 cycles par configuration de 1, 2 et
4 workers, quatre producteurs, 128 jobs par cycle : **76 800 jobs** comptés
exactement une fois par plateforme. Il inclut idle, wake waves, paniques
injectées et shutdown, plus les sept autres scénarios ciblés de la suite.

Miri : nightly-2026-10-02, graines 20261003 et 20261004.
La suite --lib --test miri_core conserve les vérifications de provenance,
d'emprunts et de fuite. disable-isolation autorise les fixtures sysfs temporaires.
Les appels Linux sched_setaffinity/mmap/mbind/mincore/munmap sont exclus par
cfg(miri) ; les opérations de file et les effacements de durée de vie restent
exécutés. Un seul test sysfs est ignoré sous Miri, avec son motif dans le code :
ouvrir un dossier comme un fichier n'est pas pris en charge.
Les longs tests d'intégration natifs et leurs timeouts sont remplacés sous Miri
par les cas bornés du fichier miri_core ; ce n'est pas une preuve exhaustive
de tous les entrelacements du runtime.

Sept tests réels NUMA restent ignorés par défaut et leur tentative d'exécution
sous WSL échoue faute de /sys/devices/system/node/online. Les tests de mappings,
d'alignement et de libération Linux passent ; le constructeur NUMA réel exige
un hôte adapté. Le jalon matériel correspondant reste ouvert.


Résultats finaux : Windows 83 tests fonctionnels (84 avec scheduler-metrics) et 7 doctests ; Linux 136 (137 avec scheduler-metrics), 8 doctests et 7 tests NUMA ignorés. Clippy toutes features et rustdoc strict passent sur les deux plateformes. Miri valide 53 tests unitaires et 4 tests ciblés par graine, avec une exclusion sysfs. La feature de compteurs est testée séparément et absente du passage de débit.

## Rejouer les validations

Depuis la racine, sur chaque plateforme native :
~~~text
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo test --locked --all-targets --all-features
cargo test --locked --doc
cargo test --locked --release
cargo rustdoc --locked --lib -- -D warnings -D missing-docs
cargo run --locked --example tour
~~~

Pour le stress release, fixer WEAVE_STRESS_SEED=20261003 et
WEAVE_STRESS_ROUNDS=200 dans l'environnement puis exécuter
cargo test --locked --release --test safety_stress.

Pour Miri, installer nightly-2026-10-02 avec le composant miri,
fixer MIRIFLAGS="-Zmiri-disable-isolation -Zmiri-seed=20261003" puis exécuter
cargo +nightly-2026-10-02 miri test --locked --lib --test miri_core.
Rejouer avec la graine 20261004. Les validations WSL utilisent --offline
et des CARGO_TARGET_DIR séparés des artefacts Windows.
