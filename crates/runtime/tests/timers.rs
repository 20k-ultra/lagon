use lagon_runtime_http::{Request, Response, RunResult};
use lagon_runtime_isolate::options::IsolateOptions;
use serial_test::serial;

mod utils;

#[tokio::test]
async fn set_timeout() {
    utils::setup();
    let (send, receiver) = utils::create_isolate(IsolateOptions::new(
        "export async function handler() {
    const test = await new Promise((resolve) => {
        setTimeout(() => {
            resolve('test');
        }, 100);
    });
    return new Response(test);
}"
        .into(),
    ));
    send(Request::default());

    assert_eq!(
        receiver.recv_async().await.unwrap(),
        RunResult::Response(Response::from("test"))
    );
}

#[tokio::test]
#[serial]
async fn set_timeout_not_blocking_response() {
    utils::setup();
    let log_rx = utils::setup_logger();
    let (send, receiver) = utils::create_isolate(
        IsolateOptions::new(
            "export async function handler() {
    console.log('before')
    setTimeout(() => {
        console.log('done')
    }, 100);
    console.log('after')

    return new Response('Hello!');
}"
            .into(),
        )
        .metadata(Some(("".to_owned(), "".to_owned()))),
    );
    send(Request::default());

    assert_eq!(log_rx.recv_async().await.unwrap(), "before".to_string());
    assert_eq!(
        receiver.recv_async().await.unwrap(),
        RunResult::Response(Response::from("Hello!"))
    );
    assert_eq!(log_rx.recv_async().await.unwrap(), "after".to_string());
}

#[tokio::test]
async fn set_timeout_clear() {
    utils::setup();
    let (send, receiver) = utils::create_isolate(IsolateOptions::new(
        "export async function handler() {
    let id;
    const test = await new Promise((resolve) => {
        id = setTimeout(() => {
            resolve('first');
        }, 100);
        setTimeout(() => {
            resolve('second');
        }, 200);
        clearTimeout(id);
    });
    return new Response(test);
}"
        .into(),
    ));
    send(Request::default());

    assert_eq!(
        receiver.recv_async().await.unwrap(),
        RunResult::Response(Response::from("second"))
    );
}

#[tokio::test]
async fn set_timeout_clear_correct() {
    utils::setup();
    let (send, receiver) = utils::create_isolate(IsolateOptions::new(
        "export async function handler() {
    const test = await new Promise((resolve) => {
        setTimeout(() => {
            resolve('first');
        }, 100);
        const id = setTimeout(() => {
            resolve('second');
        }, 200);
        clearTimeout(id);
    });
    return new Response(test);
}"
        .into(),
    ));
    send(Request::default());

    assert_eq!(
        receiver.recv_async().await.unwrap(),
        RunResult::Response(Response::from("first"))
    );
}

#[tokio::test]
#[serial]
async fn set_interval() {
    let log_rx = utils::setup_logger();
    utils::setup();
    let (send, receiver) = utils::create_isolate(
        IsolateOptions::new(
            "export async function handler() {
    await new Promise(resolve => {
        let count = 0;
        const id = setInterval(() => {
            count++;
            console.log('interval', count);

            if (count >= 3) {
                clearInterval(id);
                resolve();
            }
        }, 100);
    });

    console.log('res');
    return new Response('Hello world');
}"
            .into(),
        )
        .metadata(Some(("".to_owned(), "".to_owned()))),
    );
    send(Request::default());

    assert_eq!(log_rx.recv_async().await.unwrap(), "interval 1".to_string());
    assert_eq!(log_rx.recv_async().await.unwrap(), "interval 2".to_string());
    assert_eq!(log_rx.recv_async().await.unwrap(), "interval 3".to_string());
    assert_eq!(log_rx.recv_async().await.unwrap(), "res".to_string());
    assert_eq!(
        receiver.recv_async().await.unwrap(),
        RunResult::Response(Response::from("Hello world"))
    );
}

#[tokio::test]
#[serial]
async fn queue_microtask() {
    let log_rx = utils::setup_logger();
    utils::setup();
    let (send, receiver) = utils::create_isolate(
        IsolateOptions::new(
            "export async function handler() {
    queueMicrotask(() => {
        console.log('microtask');
    });

    console.log('before')

    return new Response('Hello world');
}"
            .into(),
        )
        .metadata(Some(("".to_owned(), "".to_owned()))),
    );
    send(Request::default());

    assert_eq!(log_rx.recv_async().await.unwrap(), "before".to_string());
    assert_eq!(log_rx.recv_async().await.unwrap(), "microtask".to_string());
    assert_eq!(
        receiver.recv_async().await.unwrap(),
        RunResult::Response(Response::from("Hello world"))
    );
}

#[tokio::test]
#[serial]
async fn timers_order() {
    let log_rx = utils::setup_logger();
    utils::setup();
    let (send, receiver) = utils::create_isolate(
        IsolateOptions::new(
            "export async function handler() {
    queueMicrotask(() => {
        console.log('microtask')
    })

    Promise.resolve().then(() => {
        console.log('promise')
    })

    console.log('main');

    await new Promise(resolve => setTimeout(() => {
        console.log('timeout')
        resolve()
    }, 0))

    console.log('main 2');

    return new Response('Hello world');
}"
            .into(),
        )
        .metadata(Some(("".to_owned(), "".to_owned()))),
    );
    send(Request::default());

    assert_eq!(log_rx.recv_async().await.unwrap(), "main".to_string());
    assert_eq!(log_rx.recv_async().await.unwrap(), "microtask".to_string());
    assert_eq!(log_rx.recv_async().await.unwrap(), "promise".to_string());
    assert_eq!(log_rx.recv_async().await.unwrap(), "timeout".to_string());
    assert_eq!(log_rx.recv_async().await.unwrap(), "main 2".to_string());
    assert_eq!(
        receiver.recv_async().await.unwrap(),
        RunResult::Response(Response::from("Hello world"))
    );
}
