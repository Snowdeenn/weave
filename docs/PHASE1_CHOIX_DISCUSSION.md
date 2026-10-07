# Weave — Décisions de conception pour la phase 1

Date : 6 octobre 2026.

Ce document restitue la discussion préparatoire à la phase 1 de la roadmap. Il décrit les décisions acceptées, les explications qui les motivent et les propositions de départ. Il ne constitue ni une preuve de correction, ni une validation de performances. Aucun changement au runtime n'a été réalisé pendant cette discussion.

## 1. Objectif et ordre des travaux

La phase 1 vise à retirer les goulots d'étranglement du scheduler, principalement son mutex central, en préservant la sûreté, les priorités, les scopes, l'attente coopérative et l'arrêt propre du pool.

L'utilisateur souhaite une implémentation propre à Weave pour comprendre les atomiques et les ordres mémoire. La roadmap proposait d'évaluer d'abord une primitive éprouvée ; la décision consiste à étudier un algorithme connu, puis à l'implémenter dans Weave, plutôt qu'à inventer simultanément un algorithme et sa preuve.

Ordre retenu :

1. Concevoir et valider les nouvelles structures de stockage isolément.
2. Les intégrer en retirant le mutex central du chemin courant, avec admission et réveil corrects.
3. Mesurer cette première architecture.
4. Remanier le stealing, puis optimiser l'attente et les réveils.

Un protocole de réveil correct doit accompagner l'intégration ; seule son optimisation peut être reportée. Les optimisations de fork/join et des itérateurs restent en phase 2.

## 2. État actuel lu dans le dépôt

Sources consultées dans `D:\projects\weave` : `docs/ROADMAP.md`, `src/job.rs`, `src/pool.rs`, `src/scope.rs`, `src/handle.rs` et `src/steal.rs`.

### Stockage et propriété

Chaque worker possède actuellement trois `VecDeque<Job>`, une par priorité. Le scheduler possède également trois files globales. Les queues stockent directement les valeurs `Job` ; elles sont toutes protégées par le même mutex central.

Un `Job` contient :

- Une closure `Box<dyn FnOnce() + Send + 'static>`.
- Un objet `Completion`, comportant un nettoyage optionnel et un signal d'achèvement optionnel.
- Une priorité et un label optionnel.

La soumission déplace le job dans une queue ; le retrait le déplace vers un exécuteur ; `Job::run(self)` le consomme. Le job n'est pas cloné ni relancé.

### Scopes et achèvement

Les tâches scoped peuvent emprunter des données. Leur durée de vie est effacée pour le stockage, sous un contrat interne de sûreté. Un ticket de groupe est conservé dans `Completion`, hors de la closure effacée. Il est réglé après la sortie effective de la closure et la destruction de ses captures, y compris sur les chemins de rejet. Un résultat prêt ne suffit pas à prouver que tous les emprunts ont cessé.

La nouvelle architecture doit transporter le `Job` entier et préserver ce mécanisme.

### Scheduler et notifications

Le mutex protège aussi `pending`, le shutdown et les statistiques de vol. `pending` compte les tâches en queue et celles retirées dont l'exécution n'a pas encore été comptabilisée comme achevée.

Le parcours actuel, pour chaque priorité de High vers Low, cherche dans la queue locale, la queue globale, puis les victimes. Le retrait local est LIFO et le vol prend les tâches anciennes.

Tous les workers utilisent une condition variable partagée : soumission avec `notify_one`, fin de tâche avec `notify_all`, arrêt avec `notify_all`. Le mutex commun coordonne vérification des queues et endormissement.

## 3. Notions expliquées pendant la discussion

### Publication

Publier une tâche signifie la rendre accessible après son initialisation. Écrire son contenu et annoncer sa disponibilité sont deux actions distinctes. Un lecteur qui voit la disponibilité doit aussi disposer de la visibilité nécessaire sur le contenu.

`Relaxed` fournit l'atomicité sans publier à lui seul les autres accès mémoire. Une publication `Release`, observée par une opération `Acquire` correspondante, établit des garanties de visibilité sur les écritures précédentes. `SeqCst` ajoute un ordre commun aux opérations concernées mais ne remplace pas un algorithme correct.

Les autres workers accèdent au même état partagé ; ils lisent les indices atomiques, sans demander la longueur au propriétaire par message. Une longueur déduite de lectures concurrentes est une observation, pas une réservation.

### Arbitrage et propriété

Un CAS, `compare_exchange` en Rust, modifie un atomique seulement si sa valeur correspond encore à la valeur attendue. Il départage plusieurs tentatives visant la même position. Observer une tâche ne donne pas encore le droit de l'exécuter.

### Réutilisation et générations

Un buffer circulaire réutilise ses cases : la position logique avance, tandis que la case physique vaut la position modulo la capacité. Une génération distingue les occupations successives d'une même case, comme dans une arena générationnelle.

Mais vérifier une génération puis accéder à un contenu non protégé ne suffit pas : celui-ci peut changer entre les deux. La génération traite l'identité ; le protocole concurrent traite l'exclusivité et la durée de vie.

Une représentation pédagogique « libre → prête → réservée → libre » a été discutée. Elle n'est pas le protocole retenu pour Chase–Lev et ne doit pas être combinée arbitrairement avec celui-ci.

## 4. Deques locales : Chase–Lev extensible

### Décision acceptée

Une implémentation Chase–Lev propre à Weave, avec :

- Un seul propriétaire autorisé à faire `push` et `pop`.
- Plusieurs voleurs autorisés à faire `steal`.
- LIFO local côté `bottom`, vol des tâches anciennes côté `top`.
- Trois deques par worker pour les priorités.
- Buffer circulaire extensible ; capacité doublée à chaque agrandissement.
- Aucune réduction de capacité dans la première version.
- Conservation des anciens buffers jusqu'à destruction, après cessation de tous les accès.

Une capacité fixe avec repli vers l'injector a été envisagée, puis écartée au profit de la version extensible.

### Dernière tâche et barrières

Quand plusieurs tâches restent, propriétaire et voleurs travaillent à des extrémités différentes. Quand il reste une seule tâche, le propriétaire doit participer au même arbitrage CAS que les voleurs.

Les indices atomiques seuls ne suffisent pas : des observations croisées anciennes de `top` et `bottom` pourraient conduire le propriétaire et un voleur à prendre la même tâche. Les barrières prévues par l'algorithme empêchent cette combinaison. Les ordres exacts restent à justifier dans l'implémentation Rust, pas à déduire d'une règle générale « Acquire en lecture, Release en écriture ».

### Lecture avant CAS

Le modèle initial « réussir le CAS, puis lire la case » a été corrigé pendant la discussion : il permettrait une réutilisation avant la récupération par un gagnant suspendu.

Dans la variante étudiée de Chase–Lev, le voleur lit atomiquement la valeur de la case avant le CAS. S'il réussit, il obtient le droit de la récupérer ; s'il échoue, il abandonne la valeur observée.

Représentation retenue comme base : cases `AtomicPtr<Job>` pointant vers des `Box<Job>`. Copier un pointeur ne déplace pas la closure et n'autorise ni déréférencement, ni destruction. La prise de propriété intervient seulement après arbitrage réussi. Le protocole doit traiter les limites et le débordement des indices pour éviter une réapparition abusive d'une ancienne valeur.

Cette représentation ajoute une allocation par job, en plus de celle de la closure. Son coût fait partie des mesures.

### Agrandissement et destruction

Les pointeurs des positions disponibles sont copiés vers le nouveau buffer, sans créer de nouveaux jobs. Les buffers partagent le même arbitrage logique par indices. Les anciens buffers ne sont pas propriétaires des jobs et ne doivent jamais détruire leurs pointeurs résiduels.

Conserver les anciens buffers évite d'introduire une récupération mémoire concurrente dès la première version. Avec un doublement, la somme des anciennes capacités reste inférieure à la capacité courante, hors détails d'allocation. La mémoire du pic est conservée jusqu'à destruction.

À l'arrêt définitif, les jobs réellement restants doivent être détruits une seule fois ; les copies périmées dans les buffers ne doivent pas être parcourues comme des propriétaires.

## 5. Injector global : MPMC borné avec secours

### Rôle

Une soumission provenant d'un worker du même pool rejoint sa deque locale. Une soumission extérieure, ou provenant d'un autre pool, doit emprunter une entrée concurrente distincte. Chase–Lev ne permet pas plusieurs producteurs sur une même deque.

### Alternatives examinées

- FIFO sous mutex dédié : simple, mais contention possible entre producteurs externes.
- Channel MPMC : utilisable avec réception non bloquante, mais son attente et sa fermeture doivent rester compatibles avec le pool.
- Buffer circulaire MPMC borné : stockage fixe, réservations et séquences atomiques.
- File MPMC chaînée : extensible, avec allocations et récupération mémoire délicate.
- File segmentée : allocations amorties, gestion des blocs plus complexe.
- Files MPSC d'entrée par worker : distribution explicite et risque de déséquilibre.
- Plusieurs injectors répartis : contention distribuée mais recherche et ordre plus complexes.

### Décision acceptée

Un buffer circulaire MPMC borné propre à Weave, avec débordement vers une file extensible sous mutex dédié. Proposition d'organisation retenue dans la spécification initiale : trois buffers, et un mutex protégeant trois files de secours.

Une soumission tente le buffer ; si celui-ci est plein, elle utilise le secours. Elle ne bloque pas en attendant une case. Cela conserve une API sans erreur de capacité et évite un blocage de progression à un worker.

Le job n'est compté qu'une fois dans `pending`, quelle que soit sa destination. L'échec d'allocation du secours reste à traiter explicitement.

### Séquences des cases

Chaque case contient un emplacement de job et un numéro de séquence atomique. Pour une capacité de quatre, la case physique zéro accueille successivement les positions logiques zéro, quatre, huit, etc.

- Séquence 0 : libre pour l'insertion à la position 0.
- Séquence 1 : prête pour le retrait à la position 0.
- Séquence 4 : libre pour l'insertion à la position 4.
- Séquence 5 : prête pour le retrait à la position 4.

Les indices physiques vont de zéro à trois, mais les séquences ne se limitent pas à cet intervalle. Le consommateur annonce la prochaine disponibilité avec la position plus la capacité.

Le producteur réserve, écrit, puis publie ; le consommateur observe, réserve, déplace le job, puis publie la libération. Cette exclusivité permet d'étudier un stockage direct du `Job` dans la case, contrairement aux pointeurs des deques.

### Publication inachevée

Un producteur suspendu après réservation peut bloquer une position de retrait, même si une position suivante est publiée. Une telle queue n'est pas automatiquement lock-free parce qu'elle utilise des atomiques.

Les résultats internes doivent permettre de distinguer absence de tâche récupérable et publication en cours, selon les possibilités de l'algorithme choisi. Un worker cherche ailleurs plutôt que d'attendre indéfiniment. Le job réservé reste compté ; sa publication ultérieure participe au réveil. Le secours traite le manque de capacité, pas cette suspension.

## 6. Admission, compteur et arrêt

### Alternatives et décision

Trois options ont été discutées : petit verrou d'admission, état atomique combiné, compteurs distribués avec détection de terminaison. L'utilisateur a choisi l'état atomique combinant phase et nombre de tâches enregistrées.

États conceptuels : ouvert, drainage, terminé. La répartition des bits et les transitions exactes restent à spécifier.

- Ouvert avec zéro tâche : workers inactifs, nouvelles soumissions possibles.
- Ouvert avec travail : soumissions acceptées selon le protocole.
- Drainage : nouvelles soumissions externes refusées ; descendants des tâches déjà acceptées encore autorisés.
- Drainage avec zéro tâche enregistrée : terminaison possible.

Le pool vide ne s'arrête donc pas spontanément. Dans l'implémentation actuelle, la destruction du pool demande l'arrêt. Un objet détruit ne peut plus être utilisé pour soumettre.

### Ordre de soumission

Préparer les ressources potentiellement faillibles, enregistrer atomiquement le job, puis publier. Le compteur inclut les jobs enregistrés mais pas encore publiés. Un échec après enregistrement exige un garde qui détruit le job et annule l'enregistrement exactement une fois.

Le parent reste compté pendant la soumission de ses descendants. Le retrait d'une queue ne décrémente pas `pending` ; l'achèvement après exécution et nettoyage le fait.

L'état combiné retire le verrou mais reste un atomique chaud partagé : sa contention doit être mesurée. Les compteurs distribués sont reportés jusqu'à justification par les mesures.

## 7. Priorités et recherche

### Cycle accepté

Chaque worker suit son propre cycle : 15 High, puis 15 Normal, puis 15 Low, puis recommence. Un retrait réussi consomme une unité. Une priorité sans travail récupérable après le parcours prévu est passée. Pas de réservation anticipée de quinze jobs.

L'attente coopérative partage le cycle du worker. Les quotas comptent les jobs et non leur durée. Sous charge permanente des trois priorités, les parts de sélection sont égales : High passe d'abord dans le cycle mais ne reçoit pas davantage de sélections. Un High arrivé en phase Normal peut attendre le reste de Normal puis Low. Ces conséquences ont été exposées et acceptées pour mesure.

### Parcours initial accepté

Pour la priorité courante : deque locale, injector, victimes. L'injector alterne la source essayée en premier, buffer ou secours, puis essaie l'autre si nécessaire. Cette alternance est locale et ne garantit pas à elle seule une borne temporelle d'équité.

On conserve les ordres de victimes existants : circulaire sans topologie ; proximité de cœur, même NUMA, puis distant avec placement. Un job est volé à la fois. Une tentative perdue n'est pas équivalente à une deque vide ; les répétitions restent bornées.

Le vol par lots et l'adaptation à la charge sont reportés après intégration et mesures. Un vol par lots demande un protocole distinct ; il ne découle pas du quota quinze.

## 8. Attente et réveil

### Décisions acceptées

Recherche active, spin borné, yield borné, puis parking. Retour au travail dès qu'un job est récupéré.

Chaque worker possède un état de notification, un petit mutex et une condition variable. Il s'annonce candidat au sommeil, revérifie le travail, puis attend sous un protocole de prédicat. Le producteur publie le job avant de notifier. L'indicateur conserve une notification arrivée avant l'attente effective.

Une publication réveille au plus un worker endormi ; le shutdown réveille tous les workers. La mobilisation du pool lors de nombreuses publications locales doit être mesurée.

### Sélection du destinataire

Options étudiées : parcours des états, masque de bits, pile/file de dormeurs, condition variable partagée. Choix accepté : parcours des états atomiques avec départ tournant.

Les états atomiques servent à trouver un candidat, pas à prouver le réveil. Si celui-ci se désinscrit pendant la sélection, la recherche continue selon le protocole. Le mutex individuel et le prédicat assurent la notification. Un départ local au producteur est proposé pour éviter un compteur central supplémentaire.

Le protocole complet doit démontrer qu'une publication ne peut laisser tous les workers endormis par notification perdue. Une simple suite « s'annoncer, vérifier, dormir » ne constitue pas cette preuve.

## 9. Paramètres proposés, non mesurés

Ces valeurs ont été proposées pour démarrer ; elles ne sont pas des optimums validés ni des exigences publiques :

- Deque : capacité initiale 64 par priorité, allocation au premier usage.
- Injector : capacité 256 par priorité.
- Après un parcours complet infructueux : quatre recherches espacées par `spin_loop`, puis deux précédées de `yield_now`, puis parking.
- Après un vol perdu : passer à la victime suivante.

Le comportement de la première allocation, les limites de taille, les échecs d'agrandissement et le débordement des indices doivent être explicitement spécifiés.

## 10. Invariants et validation

### Invariants requis

- Une tâche acceptée est enregistrée avant publication et réglée exactement une fois.
- Un job a un seul propriétaire ; une copie de pointeur n'est pas un propriétaire.
- Aucun ancien buffer n'est libéré pendant un accès possible.
- Une tâche scoped ne survit pas à son scope ; le signal d'achèvement conserve son ordre actuel.
- Une opération propriétaire finit avant exécution de code utilisateur ou destruction de captures potentiellement réentrantes.
- Le worker reste l'unique propriétaire de ses opérations locales, y compris pendant l'attente coopérative.
- Le drainage ne termine pas tant qu'une tâche ou une publication enregistrée peut produire du travail.
- Les réveils ne sont pas perdus ; les attentes tolèrent les réveils parasites.
- Les limites des compteurs et les chemins de rejet sont définis.

### Scénarios

Deque : voleurs simultanés, dernière tâche, suspension avant/après arbitrage, réutilisation des cases, agrandissement concurrent au vol, destruction.

MPMC : producteurs/consommateurs multiples, plusieurs tours, buffer plein, secours, réservation non publiée, libération et réutilisation.

Runtime : un worker, plusieurs pools, soumissions externes concurrentes, scopes, attente imbriquée, panics, rejets, destruction des captures, shutdown avec descendants, courses publication/parking.

Modèles Loom ciblés et stress reproductibles doivent vérifier pertes, doublons, blocages et destructions. La primitive réelle et sa représentation mémoire doivent être reliées aux modèles ; on ne prétend pas modéliser tout le runtime.

### Mesures

Comparaison avec la référence de phase 0 : débit, latence par priorité, allocations, un worker, petits jobs, producteurs externes, charges irrégulières et progression sous charge continue. Mesurer particulièrement l'allocation `Box<Job>`, l'état atomique partagé, le débordement, les quotas 15/15/15 et le parcours des dormeurs.

## 11. Ce qui reste à préciser pendant la conception détaillée

La discussion d'architecture initiale est terminée. La phase 1 n'est pas implémentée.

Restent à démontrer ou fixer : algorithme MPMC précis, ordres mémoire Rust, propriété/provenance des pointeurs, publication des nouveaux buffers, traitement des indices et des capacités limites, transitions atomiques du pool, protocoles exacts de parking, réentrance TLS et chemins d'échec.

Ces détails ne doivent pas être remplacés par des suppositions issues des schémas pédagogiques. Les paramètres seront réévalués après mesure ; le vol par lots et les heuristiques avancées restent ouverts dans la phase 1.

## 12. Références étudiées

- David Chase et Yossi Lev, *Dynamic Circular Work-Stealing Deque* : https://www.cs.wm.edu/~dcschmidt/PDF/work-stealing-dequeue.pdf
- Nhat Minh Lê, Antoniu Pop, Albert Cohen et Francesco Zappa Nardelli, *Correct and Efficient Work-Stealing for Weak Memory Models*, 2013 : https://fzn.fr/readings/ppopp13.pdf

Ces articles servent de référence algorithmique. Leur traduction vers les types, allocations et durées de vie Rust de Weave exige une justification spécifique.
