struct Worker {
    int ready;
};

static void prepare(void) {}

int run_worker(void) {
    prepare();
    return 0;
}

