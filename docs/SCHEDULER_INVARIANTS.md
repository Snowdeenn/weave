# Invariants du scheduler

Ce document décrit le scheduler du socle référencé par
[la vérification](verification/README.md), fondé sur le commit
`f0f81d30b561cfdfe70511788362770904af2ff1`.
Le checkout ajoute le nettoyage sur rejet et une barrière de fin hors de l'appel FnOnce ; le scheduler conserve ses files et sa politique de sélection.
Implémentation : [pool.rs](../src/pool.rs), [job.rs](../src/job.rs),
[handle.rs](../src/handle.rs) et [scope.rs](../src/scope.rs).

## 1. Propriété exclusive des jobs

Un `Job` possède sa closure `Box<dyn FnOnce() + Send>`. Il n'est pas clonable.
Son parcours est un transfert de propriété :

```text
appelant → enqueue → une file → take → un exécuteur → run(self) → destruction
```

Un job accepté appartient exactement à une file ou à un exécuteur. Un vol retire
le job de la file de la victime et le transfère directement à l'exécuteur ;
il ne crée pas de copie et ne republie pas le job. Les files locales ne sont pas
réservées à l'accès de leur worker : tous leurs accès passent par le même mutex.

Un exécuteur est soit la boucle d'un worker, soit l'aide coopérative d'un worker
pendant `join`, `scope` ou `install`. Plusieurs jobs peuvent donc être retirés
et encore inachevés sur la pile d'un même worker. Le nombre de jobs en cours ne
se déduit pas du nombre de threads.

Le `JoinHandle` partage uniquement l'état du résultat. Il ne possède ni la
closure en file ni le droit de la retirer. Son abandon n'annule pas le job.

## 2. Acceptation et publication

Une tâche est **acceptée** lorsque `enqueue` a déplacé son job dans une file et
incrémenté `pending`, sous le mutex `scheduler`. Ces deux changements forment
une seule transition visible aux autres accès au scheduler. Une closure
construite mais non publiée n'est pas encore une tâche acceptée.

Le choix de la file respecte l'identité du pool : seul un worker dont
`SharedPoolData` est identique à celui du destinataire publie dans sa file
locale. Un appel externe ou depuis un autre pool publie dans la file globale.
La priorité sélectionne l'une des trois files.

La libération du mutex après publication, puis sa prise par le consommateur,
assurent la visibilité du job et de ses captures transférées. `notify_one`
réveille un worker ; la notification n'est pas le mécanisme de publication.

Le worker vérifie les files et la condition d'arrêt sous le même mutex, puis
`Condvar::wait` libère ce mutex et attend atomiquement. Le producteur ne peut
donc pas publier entre la vérification et la mise en attente en perdant le réveil.
Tout réveil, y compris spontané, entraîne une nouvelle vérification en boucle.

## 3. Exécution au plus une fois

`Scheduler::take` retire un job par un `pop` sous le mutex commun. Deux
consommateurs, y compris deux voleurs, ne peuvent pas obtenir le même job.
Après ce retrait, l'exécuteur est son unique propriétaire ; `Job::run(self)`
consomme ce propriétaire et appelle la closure `FnOnce`.

Le chemin normal appelle `execute` une seule fois pour chaque job retiré.
Il n'existe ni remise en file, ni nouvelle tentative après panique, ni annulation.
Les priorités et le plan de victimes changent le choix du prochain job,
pas son identité ou sa propriété. « Au plus une fois » concerne chaque job :
publier deux closures effectuant la même opération produit deux jobs distincts.

Le verrou du scheduler est relâché avant l'exécution du code utilisateur.
Une closure peut ainsi publier des descendants ou exécuter de l'aide coopérative
sans réentrer dans un mutex déjà détenu par cette exécution.

## 4. Comptabilité et achèvement

Aux frontières des transitions protégées par le mutex, en production :

```text
pending = jobs en file + jobs retirés dont execute n'a pas encore comptabilisé la fin
```

- `enqueue` : ajoute un job en file et incrémente `pending` une fois.
- `take` : transfère un job à l'exécuteur sans modifier `pending`.
- `execute` : consomme le job, traite un éventuel unwind, puis décrémente
  `pending` une fois et appelle `notify_all`.

Le compteur inclut un parent suspendu pendant que l'aide coopérative exécute
un enfant. Un parent publiant un descendant reste compté jusqu'à la fin de
son propre `execute`. Sous les hypothèses ci-dessous, le compteur ne devient
ni négatif ni nul tant qu'un job accepté peut encore publier des descendants.
Les fixtures des tests unitaires du scheduler manipulent directement les files
sans cette comptabilité ; cette équation ne leur est pas applicable.

Une panique Rust de la closure, avec déroulement de pile, est interceptée.
Une panique du destructeur du payload est aussi interceptée par `discard`
avant la décrémentation ; un payload de cette seconde panique est oublié.
Cela ne couvre pas un double panic provoquant un abort.

Trois événements sont distincts :
1. Le résultat d'une soumission est publié par `JobState::complete`, sous son
   propre mutex ; `join` retire ce résultat. Le contrat interne est une unique
   publication par la closure soumise, pas une protection contre plusieurs
   appels arbitraires à `complete`.
2. Le groupe d'un scope comptabilise la fin de ses closures empruntées.
3. Le scheduler comptabilise la fin du wrapper complet dans `execute`.

Un résultat prêt, ou la fin d'un scope, ne signifie donc pas que le `pending`
global a déjà été décrémenté. Les captures empruntées doivent avoir fini d'être
utilisées avant la fin du groupe ou de l'attente de `join_on` ; ce sont les
invariants qui autorisent leurs effacements internes de durée de vie.

## 5. Arrêt et garantie conditionnelle d'achèvement

`stop` positionne `shutdown` et réveille les workers. Il ne vide pas les files
et ne ferme pas `enqueue`. Les descendants de jobs déjà acceptés peuvent
encore être publiés : leurs parents sont toujours comptés dans `pending`.

Un worker ne sort que si aucune tâche n'est disponible et si
`shutdown && pending == 0`, vérifiés sous le mutex. Un job retiré mais encore
en cours empêche donc la sortie, même si toutes les files sont momentanément vides.
Les dernières fins d'exécution réveillent aussi les workers en attente.

La destruction externe du pool demande l'arrêt, puis rejoint les threads.
La destruction depuis l'un de ses workers demande l'arrêt et laisse les threads
terminer en arrière-plan, pour éviter d'attendre le thread courant.
Cette seconde destruction ne garantit pas que toutes les tâches sont déjà
achevées lorsqu'elle retourne.

L'API sûre et les durées de vie doivent empêcher une nouvelle publication
après qu'il n'existe plus de worker actif ni de job compté. Tout futur accès
interne conservant `SharedPoolData` devra maintenir cette règle : `enqueue`
ne rejette pas lui-même une publication après arrêt.

Chaque tâche acceptée atteint la fin de `execute` si :
- ses callbacks et destructeurs terminent par retour ou unwind récupérable ;
- les workers restent capables de progresser, sans interblocage utilisateur ;
- aucune tâche n'est affamée indéfiniment par des publications prioritaires ;
- le processus reste vivant, les mutex restent utilisables et les compteurs
  ne débordent pas.

Pour un ensemble fini de jobs et de descendants qui terminent, avec des workers
qui progressent, le drainage finit. Pour des producteurs continus, la priorité
stricte et les files locales LIFO n'offrent aucune garantie générale d'équité.
Un appel bloquant sur un verrou ou un canal peut empêcher toute progression.
`panic=abort`, arrêt du processus et épuisement fatal de ressources sortent
également de cette garantie.

## 6. Tests associés et portée

Les tests d'intégration cités sont dans [tests/pool.rs](../tests/pool.rs) :
- `concurrent_producers_execute_every_job_exactly_once` : 4 000 jobs issus
  de producteurs concurrents, chacun observé exactement une fois après drainage.
- `idle_workers_wake_and_drop_drains_all_jobs` : réveil des workers inactifs
  et achèvement de 2 000 jobs avant le retour de la destruction externe.
- `workers_really_steal_locally_spawned_jobs` : transfert effectif d'un job
  local à un autre worker.
- `handles_report_results_panics_and_detach` : résultat, payload de panique,
  survie du worker et exécution malgré l'abandon du handle.
- `nested_submit_join_and_install_work_on_one_worker` : aide coopérative
  permettant une imbrication avec un seul worker.
- `scope_borrows_returns_values_and_waits_for_descendants`,
  `scope_waits_on_body_panic_and_propagates_task_panic` et
  `scoped_submission_panics_are_observed_or_propagated` : achèvement des
  descendants et des emprunts, y compris en cas de panique ou de handle oublié.
- `scope_catches_panicking_result_destructors_without_hanging` :
  comptabilisation du groupe malgré un destructeur de résultat qui panique.
- `pool_can_be_dropped_by_its_last_worker_owner` : absence d'auto-join et retour
  de la tâche qui détruit son pool ; ne prouve pas seul le drainage de toutes
  les autres tâches en arrière-plan.
- `priorities_select_queued_work_without_preemption` : choix par priorité.

Les tests `pool::scheduler_tests` dans [pool.rs](../src/pool.rs) vérifient
les ordres LIFO/FIFO, les victimes, les priorités et les compteurs de vol.
La [matrice](verification/features.json) relie ces tests aux fonctionnalités.
Ces cas exercent les invariants ; ils ne prouvent pas tous les entrelacements
possibles ni la vivacité de code utilisateur arbitraire.


## Publication rejetée et nettoyage des emprunts

La publication réserve la place en file et vérifie le compteur avant acceptation ; les erreurs libèrent le mutex avant de paniquer. Après acceptation, le retour ne contient aucune opération faillible. Un GroupTicket règle le compteur du scope sur exécution comme sur rejet, après destruction des captures. L'[audit de phase 0](verification/phase0/README.md) relie ces obligations aux tests de rejet et de destructeur paniqueur.

L'appel FnOnce doit avoir entièrement retourné ou déroulé sa pile avant de régler le ticket du scope ou le signal final du handle. Un guard Completion possédé par Job effectue cette transition dans la frame extérieure de run. La fin du code utilisateur dans la closure ne suffit pas : ses arguments peuvent conserver des emprunts protégés jusqu'à la sortie effective de l'appel. Miri a détecté puis validé la correction de cette distinction. JobState publie d'abord le résultat ; JoinHandle attend aussi ce signal final Release/Acquire avant de rendre le résultat.
