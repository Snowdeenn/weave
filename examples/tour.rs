use weave::{Job, Priority, ThreadPoolBuilder, WorkerLocal, iter::*};

fn main() {
    let pool = ThreadPoolBuilder::new()
        .num_threads(4)
        .thread_name("atelier")
        .build();

    // Une tâche autonome, avec une priorité et un nom descriptif.
    pool.spawn(
        Job::new(|| println!("Tâche autonome exécutée"))
            .set_priority(Priority::High)
            .set_label("accueil"),
    );

    // Un ticket pour récupérer le résultat.
    let ticket = pool.submit(|| 6 * 7);
    assert_eq!(ticket.join().unwrap(), 42);

    // Deux calculs dont on attend les deux résultats.
    let (a, b) = pool.join(|| 20, || 22);
    println!("Deux branches : {a} + {b} = {}", a + b);

    // Des tâches qui empruntent directement des variables de cette fonction.
    let mut gauche = 0;
    let mut droite = 0;
    pool.scope(|s| {
        s.spawn(|| gauche = 10);
        s.spawn(|| droite = 32);
    });
    assert_eq!(gauche + droite, 42);

    // install choisit le pool utilisé par les itérateurs.
    let entree: Vec<usize> = (0..10_000).collect();
    let mut sortie = vec![0; entree.len()];
    pool.install(|| entree.iter_parallel().map(|n| n * 2).fill(&mut sortie));
    let total = pool.install(|| {
        sortie.iter_parallel().map(|n| *n).fold(
            0,
            |somme, n| somme + n,
            |gauche, droite| gauche + droite,
        )
    });
    assert_eq!(total, 99_990_000);
    assert_eq!(
        pool.install(|| sortie.iter_parallel().map(|n| *n).reduce(|a, b| a + b)),
        Some(total)
    );
    println!("10 000 valeurs doublées, somme : {total}");

    // Un compteur distinct pour chaque worker.
    let compteurs = WorkerLocal::new(&pool, || 0usize);
    pool.install(|| {
        (0..10_000).parallelize().for_each(|_| {
            compteurs.with(|compteur| *compteur += 1);
        })
    });
    let comptes = compteurs.into_inner();
    assert_eq!(comptes.iter().sum::<usize>(), 10_000);
    println!("Répartition par worker : {comptes:?}");
    println!("Tâches volées : {}", pool.steal_count());
    // Le destructeur attend les tâches restantes.
}
